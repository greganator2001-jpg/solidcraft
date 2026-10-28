"""Rebuild a Fusion design saved by the Fission Export script (a .ffx) as a Fission timeline.

Fission runs this through fission_bridge (`FISSION_FFX` is the folder the .ffx was unpacked into).
Each Fusion timeline step becomes Fission commands: sketches keep their geometry (and Fusion's
dimensions, as parameters), features are done again with Fission's own tools, and after every
step the bodies are checked against what Fusion recorded (volume of each). Where Fusion's
feature couldn't be described (the script saved a STEP snapshot instead), the bodies are swapped
for the snapshot and the rebuild carries on from there.

If a step can't be rebuilt or comes out different, this stops with an error and Fission brings
in Fusion's finished bodies instead.

All lengths in the file are mm, angles degrees, coordinates world space.
"""
import json
import math
import os

from fission_bridge import call, message, FissionError  # noqa: F401


class Stop(Exception):
    """This step can't be rebuilt: Fission falls back to the finished bodies."""


# ---------------------------------------------------------------- small vector helpers
def sub(a, b):
    return [a[i] - b[i] for i in range(3)]


def dot(a, b):
    return sum(a[i] * b[i] for i in range(3))


def cross(a, b):
    return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]


def norm(a):
    n = math.sqrt(dot(a, a)) or 1.0
    return [c / n for c in a]


def axis_angle(x, y, z):
    """The rotation whose columns are x, y, z as (axis, degrees)."""
    r = [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]]
    tr = r[0][0] + r[1][1] + r[2][2]
    ang = math.acos(max(-1.0, min(1.0, (tr - 1) / 2)))
    if ang < 1e-9:
        return [0.0, 0.0, 1.0], 0.0
    if abs(ang - math.pi) < 1e-6:
        xx, yy, zz = ((r[i][i] + 1) / 2 for i in range(3))
        axis = [math.sqrt(max(xx, 0)), math.sqrt(max(yy, 0)), math.sqrt(max(zz, 0))]
        if axis[0] > 1e-6:
            axis[1] = math.copysign(axis[1], r[0][1])
            axis[2] = math.copysign(axis[2], r[0][2])
        elif axis[1] > 1e-6:
            axis[2] = math.copysign(axis[2], r[1][2])
        return axis, 180.0
    s = 2 * math.sin(ang)
    return [(r[2][1] - r[1][2]) / s, (r[0][2] - r[2][0]) / s, (r[1][0] - r[0][1]) / s], math.degrees(ang)


# ---------------------------------------------------------------- 2D loops (profiles)
def chain(pieces):
    """Stroked pieces of a loop, joined end to end into one polygon."""
    pieces = [list(p) for p in pieces if len(p) >= 2]
    if not pieces:
        return []
    out = pieces.pop(0)
    while pieces:
        end = out[-1]
        best, flip, bd = 0, False, None
        for i, p in enumerate(pieces):
            for f, q in ((False, p[0]), (True, p[-1])):
                d = math.dist(end, q)
                if bd is None or d < bd:
                    best, flip, bd = i, f, d
        p = pieces.pop(best)
        out.extend(reversed(p) if flip else p)
    return out


def area_centroid(poly):
    a = cx = cy = 0.0
    for (x0, y0), (x1, y1) in zip(poly, poly[1:] + poly[:1]):
        c = x0 * y1 - x1 * y0
        a += c
        cx += (x0 + x1) * c
        cy += (y0 + y1) * c
    a /= 2
    if abs(a) < 1e-12:
        return 0.0, [0.0, 0.0]
    return a, [cx / (6 * a), cy / (6 * a)]


def profile_shape(rec):
    """Area (mm²) and centroid (sketch mm) of a Fusion profile, from its loops."""
    total, mx, my = 0.0, 0.0, 0.0
    for lp in rec.get("loops") or []:
        poly = chain(lp.get("pieces") or [])
        a, c = area_centroid(poly)
        a = abs(a) * (1 if lp.get("outer") else -1)
        total += a
        mx += a * c[0]
        my += a * c[1]
    recorded = rec.get("area")
    area = recorded if isinstance(recorded, (int, float)) and recorded > 1e-9 else abs(total)
    if abs(total) < 1e-12:
        return area, [0.0, 0.0]
    return area, [mx / total, my / total]


# ---------------------------------------------------------------- the rebuild
class Rebuild:
    def __init__(self, folder):
        self.folder = folder
        with open(os.path.join(folder, "timeline.json"), encoding="utf-8") as fh:
            self.doc = json.load(fh)
        self.sketches = {}     # Fusion timeline index -> (Fission sketch id, curve map, frame)
        self.bodies = {}       # Fusion body key -> Fission body name
        self.params = set()
        self.notes = []
        # which reading of a step's data is tried (Fusion's data can be read two ways: which side an
        # extrude goes, which way a revolve or a pattern turns)
        self.variant = 0

    # ---- parameters
    def add_params(self):
        for p in self.doc.get("params") or []:
            if not p.get("user"):
                continue
            name, expr = p.get("name"), p.get("expr")
            if not name or not expr:
                continue
            try:
                call("parameters.add", {"name": name, "expression": expr, "comment": p.get("comment") or ""})
                self.params.add(name)
            except FissionError:
                pass

    def value(self, prm, unit="mm"):
        """A parameter record as Fission takes it: its expression when it only uses your own
        parameters (so the design follows them), else its value."""
        if not prm:
            raise Stop("a value is missing")
        expr = str(prm.get("expr") or "")
        words = {w for w in __import__("re").findall(r"[A-Za-z_][A-Za-z_0-9]*", expr)}
        units = {"mm", "cm", "m", "in", "ft", "deg", "rad", "um", "mil", "yd", "km", "nm", "grad",
                 "sin", "cos", "tan", "sqrt", "abs", "pi", "PI", "min", "max", "round", "floor", "ceil"}
        if expr and words <= (self.params | units):
            return expr
        v = prm.get("value")
        if not isinstance(v, (int, float)):
            raise Stop("a value Fission can't read ({})".format(expr))
        return "{} {}".format(v, unit) if unit else v

    def num(self, prm):
        v = (prm or {}).get("value")
        if not isinstance(v, (int, float)):
            raise Stop("a value Fission can't read")
        return v

    # ---- sketches
    def sketch(self, rec):
        d = rec["data"]
        frame = (d["origin"], d["x"], d["y"])
        r = call("sketch.create", {"plane": {"origin": d["origin"], "x_dir": d["x"], "y_dir": d["y"]}})
        sid = r.get("sketch")
        pts = d.get("points") or {}
        ids = {}

        def P(key):
            """A point argument: the Fission id of a point already made, else its place."""
            if key is None:
                return None
            if key in ids:
                return ids[key]
            p = pts.get(key)
            if p is None:
                return None
            if p.get("origin") and abs(p["xy"][0]) < 1e-9 and abs(p["xy"][1]) < 1e-9:
                return [0.0, 0.0]
            return list(p["xy"])

        def keep(key, made):
            if key and key not in ids and made:
                ids[key] = made

        def info(cid):
            return next((c for c in (call("sketch.inspect", {"sketch": sid}) or {}).get("curves") or [] if c.get("id") == cid), {})

        cmap = {}
        for c in d.get("curves") or []:
            t = c.get("type")
            made = None
            try:
                if t == "line":
                    a, b = P(c["a"]), P(c["b"])
                    if a is None or b is None:
                        continue
                    made = call("sketch.line", {"points": [a, b]})
                    cid = ((made or {}).get("created") or {}).get("curves", [None])[0]
                    ci = info(cid)
                    keep(c["a"], ci.get("start"))
                    keep(c["b"], ci.get("end"))
                elif t == "circle":
                    cc = P(c["c"])
                    if cc is None:
                        continue
                    made = call("sketch.circle.center", {"center": cc, "radius": float(c["r"])})
                    cid = ((made or {}).get("created") or {}).get("curves", [None])[0]
                    keep(c["c"], info(cid).get("center"))
                elif t == "arc":
                    a, b = P(c["a"]), P(c["b"])
                    mid = c.get("mid")
                    if a is None or b is None or not mid:
                        continue
                    made = call("sketch.arc.three_point", {"start": a, "end": b, "through": list(mid)})
                    cid = ((made or {}).get("created") or {}).get("curves", [None])[0]
                    ci = info(cid)
                    keep(c["a"], ci.get("start"))
                    keep(c["b"], ci.get("end"))
                    keep(c.get("c"), ci.get("center"))
                elif t == "spline":
                    s = c.get("pts") or []
                    if len(s) < 2:
                        continue
                    step = max(1, len(s) // 24)
                    fit = [list(q) for q in s[::step]]
                    if fit[-1] != list(s[-1]):
                        fit.append(list(s[-1]))
                    a, b = P(c.get("a")), P(c.get("b"))
                    if a is not None:
                        fit[0] = a
                    if b is not None and len(fit) > 1:
                        fit[-1] = b
                    made = call("sketch.spline.fit_point", {"points": fit})
                    cid = ((made or {}).get("created") or {}).get("curves", [None])[0]
                    ci = info(cid)
                    ps = ci.get("points") or []
                    if ps:
                        keep(c.get("a"), ps[0])
                        keep(c.get("b"), ps[-1])
                else:
                    continue
            except FissionError as e:
                self.notes.append("{}: a {} was left out ({})".format(rec.get("name"), t, e))
                continue
            cid = ((made or {}).get("created") or {}).get("curves", [None])[0]
            if cid:
                cmap[c["id"]] = cid
                if c.get("construction") or c.get("centerline"):
                    try:
                        call("sketch.construction", {"curves": [cid]})
                    except FissionError:
                        pass
        # text: its outline, as fitted curves
        for t in d.get("texts") or []:
            for c in t.get("curves") or []:
                s = c.get("pts") or []
                if len(s) < 3:
                    continue
                step = max(1, len(s) // 40)
                fit = [list(q) for q in s[::step]]
                closed = math.dist(s[0], s[-1]) < 1e-6
                if closed and fit[-1] != fit[0]:
                    fit.append(fit[0])
                try:
                    call("sketch.spline.fit_point", {"points": fit})
                except FissionError:
                    pass
        # Fusion's dimensions, driving the geometry by the same parameters
        self.dimensions(d, sid, ids, cmap)
        call("sketch.finish", {})
        self.sketches[rec["i"]] = (sid, cmap, frame)

    def dimensions(self, d, sid, ids, cmap):
        for dm in d.get("dims") or []:
            if not dm.get("driving", True):
                continue
            t = dm.get("type")
            prm = dm.get("param") or {}
            try:
                value = self.value(prm)
            except Stop:
                continue
            req = None
            if t == "SketchDiameterDimension" and dm.get("entity") in cmap:
                req = {"entities": [cmap[dm["entity"]]], "type": "diameter", "value": value}
            elif t == "SketchRadialDimension" and dm.get("entity") in cmap:
                req = {"entities": [cmap[dm["entity"]]], "type": "radius", "value": value}
            elif t == "SketchLinearDimension":
                pa = ids.get(dm.get("entityOne")) or cmap.get(dm.get("entityOne"))
                pb = ids.get(dm.get("entityTwo")) or cmap.get(dm.get("entityTwo"))
                if pa and pb:
                    kind = {"horizontal": "horizontal", "vertical": "vertical"}.get(dm.get("orientation"), "distance")
                    req = {"entities": [pa, pb], "type": kind, "value": value}
            if req is None:
                continue
            try:
                call("sketch.dimension", req)
            except FissionError:
                # a dimension Fission reads another way: the geometry is where Fusion had it anyway
                pass

    def profiles(self, recs):
        """Fission's profile indices for these Fusion profiles (all of one sketch)."""
        if not recs:
            raise Stop("no profiles")
        sk = self.sketches.get(recs[0].get("sketch"))
        if sk is None:
            raise Stop("its sketch wasn't rebuilt")
        sid = sk[0]
        mine = (call("sketch.inspect", {"sketch": sid}) or {}).get("profiles") or []
        picked = []
        for r in recs:
            area, centre = profile_shape(r)
            best, bd = None, None
            for p in mine:
                pa = p.get("area_mm2", 0.0)
                if abs(pa - area) > max(0.02 * area, 0.05):
                    continue
                c = p.get("centroid_sketch") or [0, 0]
                dd = math.dist(c, centre)
                if bd is None or dd < bd:
                    best, bd = p.get("index"), dd
            if best is None:
                raise Stop("a profile of the sketch didn't come out as in Fusion")
            if best not in picked:
                picked.append(best)
        return sid, picked

    # ---- bodies
    def body(self, key):
        name = self.bodies.get(key)
        if name is None:
            raise Stop("a body it uses ({}) wasn't rebuilt".format(key))
        return name

    def took(self, reply, fusion_keys):
        """Name the bodies a feature made after Fusion's (in order)."""
        made = (reply or {}).get("created_bodies") or []
        for key, name in zip(fusion_keys or [], made):
            self.bodies.setdefault(key, name)

    def check(self, rec):
        """The bodies Fission has now against what Fusion recorded after this step."""
        want = [b for b in rec.get("after") or [] if b.get("volume", 0) > 1e-9]
        r = call("inspect.measure", {}) or {}
        have = {b.get("name"): b.get("volume_mm3", 0.0) for b in r.get("bodies") or []}
        # bodies Fusion made that Fission doesn't know by name yet: match by volume
        free = [n for n in have if n not in self.bodies.values()]
        for b in want:
            if b["key"] not in self.bodies:
                m = next((n for n in free if abs(have[n] - b["volume"]) <= max(1e-3 * b["volume"], 1e-3)), None)
                if m:
                    self.bodies[b["key"]] = m
                    free.remove(m)
        for b in want:
            name = self.bodies.get(b["key"])
            v = have.get(name)
            if v is None or abs(v - b["volume"]) > max(5e-3 * b["volume"], 1e-2):
                raise Stop("{} came out different from Fusion's ({} has {:.1f} mm³, Fusion {:.1f} mm³)".format(
                    rec.get("name"), name or b["key"], v or 0.0, b["volume"]))

    # ---- features
    def op(self, data):
        return data.get("operation", "new")

    def extrude(self, rec):
        d = rec["data"]
        sid, profs = self.profiles(d.get("profiles"))
        p = {"sketch": sid, "profiles": profs, "operation": self.op(d)}
        e = d.get("extent") or {}
        mode = e.get("mode")

        def one(x, key="distance"):
            k = x.get("kind")
            if k == "distance":
                return {key: self.value(x.get("distance"))}
            if k == "through_all":
                return {"through_all": True, **({} if x.get("positive", True) else {"direction": "negative"})}
            if k == "all":
                return {"through_all": True, **({"direction": x["direction"]} if x.get("direction") in ("negative", "symmetric") else {})}
            if k == "to":
                ends = d.get("measured_end") or []
                if not ends:
                    raise Stop("an extrude 'to an object' Fusion didn't measure")
                return {key: "{} mm".format(ends[0])}
            raise Stop("extent {}".format(k))

        if mode == "symmetric":
            dist = self.value(e.get("distance"))
            if not e.get("full"):
                dist = "2 * ({})".format(dist)
            p.update(distance=dist, direction="symmetric")
        elif mode == "two":
            p.update(one(e["one"]))
            p.update(one(e["two"], "distance2"))
        else:
            p.update(one(e.get("one") or {}))
            dist = p.get("distance")
            # a negative one-sided distance: the other way
            if isinstance(dist, str) and dist.strip().startswith("-"):
                p["distance"] = dist.strip()[1:]
                p["direction"] = "negative"
        if self.variant == 1:
            if mode == "two":
                p["distance"], p["distance2"] = p.get("distance2"), p.get("distance")
            elif mode != "symmetric":
                if p.get("direction") == "negative":
                    p.pop("direction")
                else:
                    p["direction"] = "negative"
        if d.get("targets") and self.op(d) != "new":
            p["targets"] = [self.body(k) for k in d["targets"]]
        r = call("solid.extrude", p)
        self.took(r, d.get("result"))

    def revolve(self, rec):
        d = rec["data"]
        sid, profs = self.profiles(d.get("profiles"))
        ax = d.get("axis") or {}
        axis = None
        if ax.get("kind") == "sketchline" and ax.get("sketch") in self.sketches:
            axis = self.sketches[ax["sketch"]][1].get(ax.get("curve"))
        elif ax.get("kind") == "caxis" and ax.get("std"):
            axis = ax["std"].upper()
        if axis is None:
            raise Stop("a revolve about that kind of axis")
        p = {"sketch": sid, "profiles": profs, "axis": axis, "operation": self.op(d)}
        e = d.get("extent") or {}
        p["angle"] = self.value(e.get("angle"), "deg")
        if self.variant == 1:
            p["angle"] = "-({})".format(p["angle"])
        if d.get("targets") and self.op(d) != "new":
            p["targets"] = [self.body(k) for k in d["targets"]]
        r = call("solid.revolve", p)
        self.took(r, d.get("result"))

    @staticmethod
    def on_edge(ref):
        """A point on the edge: a circle's centroid is its centre, so take an end."""
        if ref.get("type") in ("CIRCLE", "ELLIPSE", "BSPLINE") and ref.get("ends"):
            return ref["ends"][0]
        return ref.get("mid")

    def blend(self, rec, cmd, key):
        for s in rec["data"].get("sets") or []:
            by = {}
            for e in s.get("edges") or []:
                by.setdefault(self.body(e["body"]), []).append(self.on_edge(e))
            for body, pts in by.items():
                call(cmd, {"edges": pts, key: self.value(s.get(key)), "body": body})

    def shell(self, rec):
        d = rec["data"]
        faces = d.get("faces") or []
        body = self.body(faces[0]["body"]) if faces else (self.body(d["bodies"][0]) if d.get("bodies") else None)
        if body is None:
            raise Stop("a shell of nothing")
        call("solid.shell", {"faces": [f.get("plane_point") or f.get("center") for f in faces], "body": body,
                             "thickness": self.value(d.get("thickness")), "direction": d.get("direction", "inside")})

    def targets(self, d):
        if d.get("bodies"):
            return {"bodies": [self.body(k) for k in d["bodies"]]}
        raise Stop("copying features (not bodies)")

    def plane(self, ref):
        k = ref.get("kind")
        if k == "origin":
            return {"XY": {"origin": [0, 0, 0], "normal": [0, 0, 1]}, "XZ": {"origin": [0, 0, 0], "normal": [0, 1, 0]},
                    "YZ": {"origin": [0, 0, 0], "normal": [1, 0, 0]}}[ref["name"]]
        if k == "cplane":
            return {"origin": ref["origin"], "normal": ref["normal"]}
        if k == "face":
            return {"origin": ref.get("plane_point") or ref["center"], "normal": ref.get("plane_normal") or ref["normal"]}
        raise Stop("a mirror plane Fission can't read")

    def mirror(self, rec):
        d = rec["data"]
        p = self.targets(d)
        p["plane"] = self.plane(d.get("plane") or {})
        p["combine"] = bool(d.get("combine"))
        r = call("solid.mirror", p)
        self.took(r, [k for k in d.get("result") or [] if k not in self.bodies])

    def axis(self, ax):
        if ax.get("std"):
            return ax["std"].upper()
        if ax.get("origin") and ax.get("dir"):
            return {"origin": ax["origin"], "dir": ax["dir"]}
        raise Stop("a pattern axis Fission can't read")

    def cpattern(self, rec):
        d = rec["data"]
        p = self.targets(d)
        angle = self.value(d.get("angle"), "deg")
        p.update(axis=self.axis(d.get("axis") or {}), count=int(round(self.num(d.get("count")))), angle=angle if self.variant == 0 else "-({})".format(angle))
        r = call("solid.pattern.circular", p)
        self.took(r, [k for k in d.get("result") or [] if k not in self.bodies])

    def rpattern(self, rec):
        d = rec["data"]
        p = self.targets(d)
        n1 = int(round(self.num(d.get("count1"))))
        s1 = self.num(d.get("distance1"))
        if d.get("spacing") == "extent" and n1 > 1:
            s1 = s1 / (n1 - 1)
        p.update(dir1=d["dir1"], count1=n1, spacing1=s1)
        if d.get("dir2"):
            n2 = int(round(self.num(d.get("count2"))))
            s2 = self.num(d.get("distance2"))
            if d.get("spacing") == "extent" and n2 > 1:
                s2 = s2 / (n2 - 1)
            p.update(dir2=d["dir2"], count2=n2, spacing2=s2)
        r = call("solid.pattern.rectangular", p)
        self.took(r, [k for k in d.get("result") or [] if k not in self.bodies])

    def combine(self, rec):
        d = rec["data"]
        if d.get("new_component"):
            raise Stop("a combine into a new component")
        call("solid.combine", {"target": self.body(d["target"]), "tools": [self.body(k) for k in d.get("tools") or []],
                               "operation": d.get("operation", "join"), "keep_tools": bool(d.get("keep"))})
        if not d.get("keep") and d.get("operation", "join") != "new":
            for k in d.get("tools") or []:
                self.bodies.pop(k, None)

    def move(self, rec):
        d = rec["data"]
        m = d["matrix"]
        axis, deg = axis_angle(m["x"], m["y"], m["z"])
        o = m["origin"]
        # Fusion: p' = R p + o. Fission's move turns about the origin after moving by t: R (p + t)
        x, y, z = m["x"], m["y"], m["z"]
        t = [dot(o, x), dot(o, y), dot(o, z)]
        p = {"bodies": [self.body(k) for k in d.get("bodies") or []], "translate": t}
        if abs(deg) > 1e-9:
            p.update(axis=axis, angle="{} deg".format(deg))
        call("solid.move", p)

    def snapshot(self, rec):
        """Fusion's own shape at this point (a feature Fission can't redo came before it)."""
        name = rec.get("snapshot")
        if not name:
            raise Stop("a step Fusion couldn't describe, and no snapshot of it")
        r = call("inspect.measure", {}) or {}
        mine = [b.get("name") for b in r.get("bodies") or [] if b.get("name")]
        if mine:
            call("solid.remove", {"bodies": mine})
        got = call("file.insert_step", {"path": os.path.join(self.folder, name), "name": "Fusion {}".format(name.rsplit(".", 1)[0])}) or {}
        self.bodies = {}
        new = got.get("bodies") or []
        # name them after Fusion's by volume
        m = call("inspect.measure", {"bodies": new}) or {}
        vols = {b.get("name"): b.get("volume_mm3", 0.0) for b in m.get("bodies") or []}
        for b in rec.get("after") or []:
            hit = next((n for n, v in vols.items() if n not in self.bodies.values() and abs(v - b.get("volume", 0)) <= max(1e-3 * b.get("volume", 0), 1e-3)), None)
            if hit:
                self.bodies[b["key"]] = hit
        names = {r.get("i"): r.get("name") for r in self.doc.get("timeline") or []}
        self.notes.append("{} couldn't be rebuilt step by step, so Fusion's shape at that point came in as it was".format(
            ", ".join(str(names.get(i, i)) for i in rec.get("covers") or []) or "A step"))

    def rename(self, final):
        """The bodies get Fusion's names (where no other body has that name; a few rounds, so
        names that are swapped round still land)."""
        todo = [b for b in final if self.bodies.get(b.get("key")) and b.get("name")]
        for _ in range(len(todo) + 1):
            taken = set(self.bodies.values())
            moved = False
            for b in todo:
                mine, want = self.bodies.get(b["key"]), b["name"]
                if mine == want or want in taken:
                    continue
                try:
                    call("body.rename", {"body": mine, "name": want})
                except FissionError:
                    # the name is held (by the body a Fusion snapshot replaced): close to it
                    want = "{} (Fusion)".format(want)
                    if want in taken or mine == want:
                        continue
                    try:
                        call("body.rename", {"body": mine, "name": want})
                    except FissionError:
                        continue
                taken.discard(mine)
                taken.add(want)
                self.bodies[b["key"]] = want
                moved = True
            if not moved:
                break

    TWO_WAYS = ("extrude", "revolve", "cpattern")

    def either_way(self, rec, step):
        """Do the step as recorded, checked against Fusion; if it comes out different, the other
        way round."""
        first = None
        for variant in (0, 1):
            mark = (call("edit.checkpoint", {}) or {}).get("checkpoint")
            names = dict(self.bodies)
            self.variant = variant
            try:
                step(self, rec)
                self.check(rec)
                self.variant = 0
                if mark is not None:
                    call("edit.restore_checkpoint", {"checkpoint": mark, "forget": True})
                return
            except (Stop, FissionError) as e:
                first = first or e
                self.bodies = names
                if mark is None:
                    break
                call("edit.restore_checkpoint", {"checkpoint": mark})
        self.variant = 0
        raise first if isinstance(first, Stop) else Stop("{}: {}".format(rec.get("name"), first))

    STEPS = {"extrude": extrude, "revolve": revolve, "shell": shell, "mirror": mirror, "cpattern": cpattern,
             "rpattern": rpattern, "combine": combine, "move": move}

    def run(self):
        self.add_params()
        for b in self.doc.get("start") or []:
            if b.get("volume", 0) > 1e-9:
                raise Stop("the design starts with bodies Fusion made before its timeline")
        items = self.doc.get("timeline") or []
        for rec in items:
            if rec.get("suppressed"):
                continue
            kind = rec.get("kind")
            try:
                if rec.get("type") == "_snapshot":
                    self.snapshot(rec)
                elif kind == "sketch":
                    self.sketch(rec)
                elif kind == "cplane":
                    pass  # sketches and mirrors carry their planes themselves
                elif rec.get("unsupported"):
                    if rec.get("changed"):
                        # a snapshot follows it (or the finished part covers it)
                        if not any(r.get("type") == "_snapshot" and rec.get("i") in (r.get("covers") or []) for r in items):
                            raise Stop("{}: {}".format(rec.get("name"), rec["unsupported"]))
                    continue
                elif kind == "fillet":
                    self.blend(rec, "solid.fillet", "radius")
                elif kind == "chamfer":
                    self.blend(rec, "solid.chamfer", "distance")
                elif kind in self.TWO_WAYS and rec.get("changed"):
                    self.either_way(rec, self.STEPS[kind])
                    continue
                elif kind in self.STEPS:
                    self.STEPS[kind](self, rec)
                elif rec.get("changed"):
                    raise Stop("{} ({}) isn't one Fission rebuilds yet".format(rec.get("name"), rec.get("type")))
                else:
                    continue
            except FissionError as e:
                raise Stop("{}: {}".format(rec.get("name"), e))
            if rec.get("changed") and rec.get("type") != "_snapshot" and not (rec.get("unsupported")):
                self.check(rec)
        final = (self.doc.get("final") or {}).get("after")
        if final:
            self.check({"name": "the finished part", "after": final})
            self.rename(final)
        for n in self.notes:
            message(n)


def run(context):
    folder = os.environ.get("FISSION_FFX")
    if not folder:
        raise Stop("no .ffx to rebuild")
    Rebuild(folder).run()
