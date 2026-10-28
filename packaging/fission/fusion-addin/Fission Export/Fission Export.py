"""Fission Export — a Fusion script that saves the open design *with its timeline*
so Fission can rebuild it as an editable history.

Run it from Fusion: UTILITIES > ADD-INS > Scripts and Add-Ins > Scripts > "Fission Export" > Run.
It writes one .ffx file (a zip) that you open in Fission with MANAGE > ADD-INS > Open from Fusion.

How it works
------------
Fusion only lets a script read some feature inputs (the edges a fillet used, the bodies a cut
went through) while the timeline marker sits just before that feature, so the script steps the
marker through the timeline once, from the start:

* before each feature it reads what the feature uses (sketch curves, profiles, distances, edges);
* after it, it records every body's volume, area and size, so Fission can check its own
  rebuild step by step;
* where a feature is something Fission can't redo yet (loft, sweep, draft, thread, ...) it
  saves a STEP snapshot of the bodies at that point, so the final part always comes out right.

At the end the marker goes back where it was. The design itself is never changed (some body /
component light bulbs are switched on for a moment while a snapshot is written, then restored).

All lengths in the file are millimetres, angles degrees, coordinates world (model) space.
"""
import adsk.core
import adsk.fusion
import json
import math
import os
import shutil
import tempfile
import time
import traceback
import zipfile

FORMAT = "fissioncad-fusion-export"  # the file format keeps its name: FissionCAD reads these too
VERSION = 1
CM = 10.0                       # Fusion's internal unit is the centimetre
LENGTH_UNITS = {"mm", "cm", "m", "in", "ft", "um", "µm", "mil", "km", "yd", "nm"}
ANGLE_UNITS = {"deg", "rad", "grad"}
STROKE_TOL = 0.0005             # cm; chord tolerance when sampling curves

_handlers = []


# ---------------------------------------------------------------------------- small helpers
def items(coll):
    """Iterate a Fusion collection (or a Python list / None) safely."""
    if coll is None:
        return []
    if isinstance(coll, (list, tuple)):
        return list(coll)
    out = []
    try:
        n = coll.count
    except Exception:
        try:
            return list(coll)
        except Exception:
            return []
    for i in range(n):
        try:
            out.append(coll.item(i))
        except Exception:
            pass
    return out


def kind(obj):
    """'SketchLine', 'ExtrudeFeature', ... from 'adsk::fusion::SketchLine'."""
    try:
        return obj.objectType.split("::")[-1]
    except Exception:
        return type(obj).__name__


def mm(p):
    return [p.x * CM, p.y * CM, p.z * CM]


def vec(v):
    return [v.x, v.y, v.z]


def _sub(a, b):
    return [a[0] - b[0], a[1] - b[1], a[2] - b[2]]


def _dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def _cross(a, b):
    return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]


def _norm(a):
    n = math.sqrt(_dot(a, a)) or 1.0
    return [a[0] / n, a[1] / n, a[2] / n]


class Frame:
    """A rigid placement (origin in mm + three unit axes) used to take component-space
    coordinates into world space."""

    def __init__(self, o=(0, 0, 0), x=(1, 0, 0), y=(0, 1, 0), z=(0, 0, 1)):
        self.o, self.x, self.y, self.z = list(o), list(x), list(y), list(z)

    @classmethod
    def from_matrix(cls, m):
        o, x, y, z = m.getAsCoordinateSystem()
        return cls(mm(o), vec(x), vec(y), vec(z))

    def is_identity(self):
        return (max(abs(v) for v in self.o) < 1e-9 and abs(self.x[0] - 1) < 1e-12
                and abs(self.y[1] - 1) < 1e-12 and abs(self.z[2] - 1) < 1e-12)

    def point(self, p):                 # p in mm, component space
        return [self.o[i] + p[0] * self.x[i] + p[1] * self.y[i] + p[2] * self.z[i] for i in range(3)]

    def dir(self, d):
        return [d[0] * self.x[i] + d[1] * self.y[i] + d[2] * self.z[i] for i in range(3)]


IDENTITY = Frame()


class Skip(Exception):
    """The feature can't be described for Fission; it becomes a snapshot."""


# ---------------------------------------------------------------------------- the exporter
class Exporter:
    def __init__(self, app, design, ui):
        self.app, self.design, self.ui = app, design, ui
        self.root = design.rootComponent
        self.lines = []
        self.params_used = {}
        self.workdir = tempfile.mkdtemp(prefix="fissioncad_")
        self.snapshots = 0
        self.cancelled = False

    # ---- logging
    def log(self, msg):
        self.lines.append(time.strftime("%H:%M:%S ") + str(msg))

    # ---- parameters
    def param(self, p):
        """A ModelParameter / UserParameter as {name, expr, value, unit} (value in mm / deg)."""
        if p is None:
            return None
        try:
            unit = p.unit or ""
        except Exception:
            unit = ""
        v = p.value
        if unit in LENGTH_UNITS:
            v = v * CM
        elif unit in ANGLE_UNITS:
            v = math.degrees(v)
        rec = {"name": p.name, "expr": p.expression, "value": v, "unit": unit}
        self.params_used[p.name] = rec
        return rec

    def all_params(self):
        out = []
        try:
            user = {q.name for q in items(self.design.userParameters)}
        except Exception:
            user = set()
        for p in items(self.design.allParameters):
            try:
                rec = dict(self.param(p))
                rec["user"] = p.name in user
                try:
                    rec["comment"] = p.comment or ""
                except Exception:
                    pass
                try:
                    rec["role"] = adsk.fusion.ModelParameter.cast(p).role if not rec["user"] else ""
                except Exception:
                    pass
                out.append(rec)
            except Exception as e:
                self.log("parameter skipped: %s" % e)
        return out

    # ---- assembly context
    def occ_of(self, comp):
        """The single occurrence of a component (None for the root)."""
        if comp is None or comp == self.root:
            return None
        try:
            occs = items(self.root.allOccurrencesByComponent(comp))
        except Exception:
            occs = []
        if not occs:
            raise Skip("component %s isn't placed in the design" % comp.name)
        if len(occs) > 1:
            self.log("component %s is used %d times; only the first copy is rebuilt" % (comp.name, len(occs)))
        return occs[0]

    def frame_of(self, comp):
        occ = self.occ_of(comp)
        if occ is None:
            return IDENTITY
        return Frame.from_matrix(occ.transform2)

    def ctx_frame(self, obj):
        """Frame to apply to coordinates read from obj (identity if already in world space)."""
        try:
            if obj.assemblyContext is not None:
                return IDENTITY
        except Exception:
            pass
        comp = None
        for attr in ("parentComponent", "component"):
            try:
                comp = getattr(obj, attr)
                if comp is not None:
                    break
            except Exception:
                pass
        if comp is None:
            try:
                comp = obj.body.parentComponent
            except Exception:
                comp = None
        return self.frame_of(comp) if comp is not None else IDENTITY

    # ---- bodies
    def body_key(self, body):
        """'<occurrence path>/<body name>' — the same key the stats use."""
        path = ""
        try:
            ctx = body.assemblyContext
        except Exception:
            ctx = None
        if ctx is not None:
            path = ctx.fullPathName
        else:
            try:
                comp = body.parentComponent
                occ = self.occ_of(comp)
                path = occ.fullPathName if occ is not None else ""
            except Skip:
                path = "?"
            except Exception:
                path = ""
        return path + "/" + body.name

    def all_bodies(self):
        out = [("", b) for b in items(self.root.bRepBodies)]
        for occ in items(self.root.allOccurrences):
            for b in items(occ.bRepBodies):
                out.append((occ.fullPathName, b))
        return out

    def stats(self):
        """Volume / area / bounding box of every solid body, in world space (cached while a body's
        revision doesn't change, so long timelines stay quick)."""
        out = []
        cache = self.__dict__.setdefault("_stats_cache", {})
        for path, b in self.all_bodies():
            try:
                if not b.isSolid:
                    continue
                try:
                    rid = (path, b.name, b.revisionId)
                except Exception:
                    rid = None
                if rid is not None and rid in cache:
                    rec = dict(cache[rid])
                    rec["visible"] = bool(b.isLightBulbOn)
                    out.append(rec)
                    continue
                bb = b.boundingBox
                rec = {"key": path + "/" + b.name, "name": b.name, "component": path,
                       "volume": b.volume * 1000.0, "area": b.area * 100.0,
                       "bbox": [mm(bb.minPoint), mm(bb.maxPoint)],
                       "faces": b.faces.count, "visible": bool(b.isLightBulbOn)}
                try:
                    rec["lumps"] = b.lumps.count
                except Exception:
                    pass
                if rid is not None and rid[2]:
                    cache[rid] = dict(rec)
                out.append(rec)
            except Exception as e:
                self.log("body stats failed: %s" % e)
        return out

    # ---- geometry
    def curve_centroid(self, curve3d, frame=IDENTITY):
        """Length-weighted centroid and length (mm) of a curve, like Fission's edge 'mid'."""
        ev = curve3d.evaluator
        ok, t0, t1 = ev.getParameterExtents()
        ok, pts = ev.getStrokes(t0, t1, STROKE_TOL)
        pts = [frame.point(mm(p)) for p in pts]
        if len(pts) < 2:
            return pts[0] if pts else [0, 0, 0], 0.0
        tot, acc = 0.0, [0.0, 0.0, 0.0]
        for a, b in zip(pts, pts[1:]):
            L = math.sqrt(_dot(_sub(b, a), _sub(b, a)))
            tot += L
            for i in range(3):
                acc[i] += L * (a[i] + b[i]) / 2
        if tot < 1e-12:
            return pts[0], 0.0
        return [c / tot for c in acc], tot

    def edge_ref(self, e):
        frame = self.ctx_frame(e)
        geom = e.geometry
        mid, length = self.curve_centroid(geom, frame)
        try:
            ct = geom.curveType
        except Exception:
            ct = -1
        typ = {0: "LINE", 1: "CIRCLE", 2: "CIRCLE", 3: "ELLIPSE", 4: "ELLIPSE", 6: "BSPLINE"}.get(ct, "OTHER")
        ref = {"body": self.body_key(e.body), "mid": mid, "len": e.length * CM, "type": typ}
        try:
            ok, p0, p1 = e.evaluator.getEndPoints()
            ref["ends"] = [frame.point(mm(p0)), frame.point(mm(p1))]
        except Exception:
            pass
        return ref

    def face_ref(self, f):
        frame = self.ctx_frame(f)
        c = f.centroid
        try:
            ok, n = f.evaluator.getNormalAtPoint(f.pointOnFace)
            nrm = frame.dir(vec(n))
        except Exception:
            nrm = [0, 0, 0]
        try:
            st = f.geometry.surfaceType
        except Exception:
            st = -1
        typ = {0: "PLANE", 1: "CYLINDER", 2: "CONE", 3: "SPHERE", 4: "TORUS"}.get(st, "OTHER")
        ref = {"body": self.body_key(f.body), "center": frame.point(mm(c)), "normal": nrm,
               "area": f.area * 100.0, "type": typ}
        if typ == "PLANE":
            try:
                g = f.geometry
                ref["plane_point"] = frame.point(mm(g.origin))
                ref["plane_normal"] = frame.dir(vec(g.normal))
            except Exception:
                pass
        return ref

    def plane_ref(self, ent):
        """A planar reference used by sketches, construction planes and mirrors."""
        cp = adsk.fusion.ConstructionPlane.cast(ent)
        if cp is not None:
            comp = cp.component
            for name, std in (("XY", comp.xYConstructionPlane), ("XZ", comp.xZConstructionPlane),
                              ("YZ", comp.yZConstructionPlane)):
                if cp == std:
                    frame = self.frame_of(comp)
                    return {"kind": "origin", "name": name, "moved": not frame.is_identity()}
            g = cp.geometry
            frame = self.ctx_frame(cp)
            rec = {"kind": "cplane", "name": cp.name,
                   "origin": frame.point(mm(g.origin)), "normal": frame.dir(vec(g.normal)),
                   "u": frame.dir(vec(g.uDirection))}
            try:
                rec["i"] = cp.timelineObject.index
            except Exception:
                pass
            return rec
        face = adsk.fusion.BRepFace.cast(ent)
        if face is not None:
            rec = self.face_ref(face)
            rec["kind"] = "face"
            return rec
        raise Skip("unsupported plane reference (%s)" % kind(ent))

    def axis_ref(self, ent):
        """An axis as a world-space line {origin, dir} plus what it was."""
        ca = adsk.fusion.ConstructionAxis.cast(ent)
        if ca is not None:
            comp = ca.component
            frame = self.ctx_frame(ca)
            g = ca.geometry
            rec = {"kind": "caxis", "origin": frame.point(mm(g.origin)), "dir": frame.dir(vec(g.direction)),
                   "name": ca.name}
            for name, std in (("x", comp.xConstructionAxis), ("y", comp.yConstructionAxis),
                              ("z", comp.zConstructionAxis)):
                if ca == std:
                    rec["std"] = name
            return rec
        line = adsk.fusion.SketchLine.cast(ent)
        if line is not None:
            g = line.worldGeometry
            a, b = mm(g.startPoint), mm(g.endPoint)
            frame = self.ctx_frame(line.parentSketch)
            a, b = frame.point(a), frame.point(b)
            rec = {"kind": "sketchline", "origin": a, "dir": _norm(_sub(b, a))}
            try:
                rec["sketch"] = line.parentSketch.timelineObject.index
                rec["curve"] = self._curve_id(line)
            except Exception:
                pass
            return rec
        edge = adsk.fusion.BRepEdge.cast(ent)
        if edge is not None:
            frame = self.ctx_frame(edge)
            g = edge.geometry
            rec = {"kind": "edge", "ref": self.edge_ref(edge)}
            if g.curveType == 0:
                rec["origin"] = frame.point(mm(g.startPoint))
                rec["dir"] = _norm(_sub(frame.point(mm(g.endPoint)), rec["origin"]))
            elif g.curveType in (1, 2):
                rec["origin"] = frame.point(mm(g.center))
                rec["dir"] = frame.dir(vec(g.normal))
            return rec
        face = adsk.fusion.BRepFace.cast(ent)
        if face is not None:
            g = face.geometry
            frame = self.ctx_frame(face)
            if g.surfaceType == 1:   # cylinder
                return {"kind": "face", "ref": self.face_ref(face),
                        "origin": frame.point(mm(g.origin)), "dir": frame.dir(vec(g.axis))}
        raise Skip("unsupported axis (%s)" % kind(ent))

    # ---- sketches
    def _curve_id(self, curve):
        sk = curve.parentSketch
        tok = None
        try:
            tok = curve.entityToken
        except Exception:
            pass
        ids = self._sketch_ids.get(sk.timelineObject.index) if hasattr(self, "_sketch_ids") else None
        if ids and tok in ids:
            return ids[tok]
        return None

    def nurbs(self, geom):
        """Exact B-spline data for any curve (mm, sketch space)."""
        g = geom
        try:
            if g.curveType != 6:
                g = g.asNurbsCurve
        except Exception:
            pass
        ok, poles, degree, knots, rational, weights, periodic = g.getData()
        return {"poles": [[p.x * CM, p.y * CM] for p in poles], "degree": degree, "knots": list(knots),
                "weights": list(weights) if rational else None, "periodic": bool(periodic)}

    def strokes(self, geom, z_check=True):
        ev = geom.evaluator
        ok, t0, t1 = ev.getParameterExtents()
        ok, pts = ev.getStrokes(t0, t1, STROKE_TOL)
        if z_check and any(abs(p.z) > 1e-6 for p in pts):
            return None
        return [[p.x * CM, p.y * CM] for p in pts]

    def sketch(self, sk):
        comp = sk.parentComponent
        frame = self.frame_of(comp)
        o = frame.point(mm(sk.origin))
        x = frame.dir(vec(sk.xDirection))
        y = frame.dir(vec(sk.yDirection))
        z = _cross(x, y)
        rec = {"origin": o, "x": x, "y": y, "z": z, "name": sk.name}
        try:
            rec["ref"] = self.plane_ref(sk.referencePlane) if sk.referencePlane is not None else {"kind": "none"}
        except Skip as e:
            rec["ref"] = {"kind": "none", "why": str(e)}
        except Exception:
            rec["ref"] = {"kind": "none"}

        ids = {}
        pts = {}
        dropped = []

        def pid(sp):
            if sp is None:
                return None
            try:
                tok = sp.entityToken
            except Exception:
                tok = None
            if tok is not None and tok in ids:
                return ids[tok]
            g = sp.geometry
            if abs(g.z) > 1e-6:
                return None
            k = "p%d" % (len(pts) + 1)
            pts[k] = {"xy": [g.x * CM, g.y * CM]}
            try:
                if sp.isFixed or sp.isReference:
                    pts[k]["fixed"] = True
            except Exception:
                pass
            if tok is not None:
                ids[tok] = k
            return k

        # origin point first so it's recognisable
        try:
            op = sk.originPoint
            k = pid(op)
            if k:
                pts[k]["origin"] = True
        except Exception:
            pass
        for sp in items(sk.sketchPoints):
            try:
                pid(sp)
            except Exception as e:
                dropped.append("point: %s" % e)

        curves = []
        for c in items(sk.sketchCurves):
            try:
                rec_c = self.sketch_curve(c, pid)
                if rec_c is None:
                    dropped.append(kind(c) + " (not in the sketch plane)")
                    continue
                k = "c%d" % (len(curves) + 1)
                rec_c["id"] = k
                try:
                    ids[c.entityToken] = k
                except Exception:
                    pass
                curves.append(rec_c)
            except Exception as e:
                dropped.append("%s: %s" % (kind(c), e))

        # text: its outline curves, fixed
        texts = []
        for t in items(sk.sketchTexts):
            try:
                outl = []
                for g in t.asCurves():
                    s = self.strokes(g)
                    if s is None:
                        continue
                    outl.append({"nurbs": self.nurbs(g), "pts": s})
                texts.append({"text": t.text, "height": t.height * CM, "font": t.fontName, "curves": outl})
            except Exception as e:
                dropped.append("text: %s" % e)

        self._sketch_ids[sk.timelineObject.index] = ids

        def ref_of(ent):
            if ent is None:
                return None
            try:
                tok = ent.entityToken
            except Exception:
                return None
            return ids.get(tok)

        cons = []
        for gc in items(sk.geometricConstraints):
            try:
                rec_g = {"type": kind(gc)}
                gcs = gc
                for cls in ("CoincidentConstraint", "CollinearConstraint", "ConcentricConstraint",
                            "EqualConstraint", "HorizontalConstraint", "HorizontalPointsConstraint",
                            "MidPointConstraint", "ParallelConstraint", "PerpendicularConstraint",
                            "SmoothConstraint", "SymmetryConstraint", "TangentConstraint",
                            "VerticalConstraint", "VerticalPointsConstraint", "PolygonConstraint",
                            "OffsetConstraint"):
                    if rec_g["type"] == cls:
                        gcs = getattr(adsk.fusion, cls).cast(gc)
                        break
                for attr in ("point", "entity", "line", "lineOne", "lineTwo", "curveOne", "curveTwo",
                             "entityOne", "entityTwo", "symmetryLine", "midPointCurve", "pointOne", "pointTwo"):
                    try:
                        v = getattr(gcs, attr)
                    except Exception:
                        continue
                    r = ref_of(v)
                    if r is not None:
                        rec_g[attr] = r
                cons.append(rec_g)
            except Exception as e:
                dropped.append("constraint: %s" % e)

        dims = []
        for d in items(sk.sketchDimensions):
            try:
                t = kind(d)
                rec_d = {"type": t}
                ds = getattr(adsk.fusion, t).cast(d) if hasattr(adsk.fusion, t) else d
                for attr in ("entityOne", "entityTwo", "line", "lineOne", "lineTwo", "entity",
                             "circleOne", "circleTwo"):
                    try:
                        v = getattr(ds, attr)
                    except Exception:
                        continue
                    r = ref_of(v)
                    if r is not None:
                        rec_d[attr] = r
                try:
                    rec_d["orientation"] = {0: "aligned", 1: "horizontal", 2: "vertical"}.get(ds.orientation, "aligned")
                except Exception:
                    pass
                try:
                    rec_d["driving"] = bool(d.isDriving)
                except Exception:
                    rec_d["driving"] = True
                rec_d["param"] = self.param(d.parameter)
                try:
                    tp = d.textPosition
                    rec_d["text_at"] = [tp.x * CM, tp.y * CM]
                except Exception:
                    pass
                dims.append(rec_d)
            except Exception as e:
                dropped.append("dimension: %s" % e)

        rec.update(points=pts, curves=curves, texts=texts, constraints=cons, dims=dims)
        if dropped:
            rec["dropped"] = dropped
        try:
            rec["visible"] = bool(sk.isLightBulbOn)
        except Exception:
            pass
        return rec

    def sketch_curve(self, c, pid):
        rec = {}
        try:
            rec["construction"] = bool(c.isConstruction)
        except Exception:
            rec["construction"] = False
        try:
            if c.isFixed or c.isReference:
                rec["fixed"] = True
        except Exception:
            pass
        line = adsk.fusion.SketchLine.cast(c)
        if line is not None:
            a, b = pid(line.startSketchPoint), pid(line.endSketchPoint)
            if a is None or b is None:
                return None
            rec.update(type="line", a=a, b=b)
            try:
                if line.isCenterLine:
                    rec["centerline"] = True
            except Exception:
                pass
            return rec
        circ = adsk.fusion.SketchCircle.cast(c)
        if circ is not None:
            cc = pid(circ.centerSketchPoint)
            if cc is None:
                return None
            rec.update(type="circle", c=cc, r=circ.radius * CM)
            return rec
        arc = adsk.fusion.SketchArc.cast(c)
        if arc is not None:
            cc, a, b = pid(arc.centerSketchPoint), pid(arc.startSketchPoint), pid(arc.endSketchPoint)
            if None in (cc, a, b):
                return None
            g = arc.geometry
            ev = g.evaluator
            ok, t0, t1 = ev.getParameterExtents()
            ok, m = ev.getPointAtParameter((t0 + t1) / 2)
            rec.update(type="arc", c=cc, a=a, b=b, r=arc.radius * CM, mid=[m.x * CM, m.y * CM])
            return rec
        # everything else (splines, ellipses, conics) as an exact B-spline
        g = c.geometry
        s = self.strokes(g)
        if s is None:
            return None
        sp = ep = None
        for attr in ("startSketchPoint", "endSketchPoint"):
            try:
                v = pid(getattr(c, attr))
            except Exception:
                v = None
            if attr == "startSketchPoint":
                sp = v
            else:
                ep = v
        rec.update(type="spline", a=sp, b=ep, pts=s, nurbs=self.nurbs(g), kind=kind(c))
        try:
            fps = [pid(p) for p in items(adsk.fusion.SketchFittedSpline.cast(c).fitPoints)]
            rec["fit"] = [p for p in fps if p]
        except Exception:
            pass
        return rec

    # ---- profiles
    def profile_rec(self, prof):
        sk = prof.parentSketch
        loops = []
        for lp in items(prof.profileLoops):
            pieces = []
            for pc in items(lp.profileCurves):
                g = pc.geometry
                ev = g.evaluator
                ok, t0, t1 = ev.getParameterExtents()
                ok, ps = ev.getStrokes(t0, t1, STROKE_TOL)
                # The docs don't say whether profile geometry is in sketch or model space, so
                # read it both ways and keep the one that matches the curve's sketch-space box.
                a = [(p.x, p.y, p.z) for p in ps]
                try:
                    b = []
                    for p in ps:
                        q = sk.modelToSketchSpace(p)
                        b.append((q.x, q.y, q.z))
                except Exception:
                    b = None
                pick = a
                if b is not None:
                    try:
                        bb = pc.boundingBox
                        lo, hi = (bb.minPoint.x, bb.minPoint.y), (bb.maxPoint.x, bb.maxPoint.y)

                        def miss(pts):
                            xs, ys = [p[0] for p in pts], [p[1] for p in pts]
                            return (abs(min(xs) - lo[0]) + abs(min(ys) - lo[1]) + abs(max(xs) - hi[0])
                                    + abs(max(ys) - hi[1]) + sum(abs(p[2]) for p in pts) / len(pts))
                        pick = a if miss(a) <= miss(b) else b
                    except Exception:
                        pick = a if all(abs(p[2]) < 1e-7 for p in a) else b
                pieces.append([[p[0] * CM, p[1] * CM] for p in pick])
            loops.append({"outer": bool(lp.isOuter), "pieces": pieces})
        rec = {"sketch": sk.timelineObject.index, "loops": loops}
        try:
            ap = prof.areaProperties(adsk.fusion.CalculationAccuracy.MediumCalculationAccuracy)
            rec["area"] = ap.area * 100.0
        except Exception:
            pass
        return rec

    def profiles(self, feat):
        p = feat.profile
        profs = []
        if adsk.fusion.Profile.cast(p) is not None:
            profs = [adsk.fusion.Profile.cast(p)]
        elif adsk.fusion.BRepFace.cast(p) is not None:
            raise Skip("the profile is a body face, not a sketch")
        else:
            for x in items(p):
                pr = adsk.fusion.Profile.cast(x)
                if pr is None:
                    raise Skip("one of the profiles is a body face, not a sketch")
                profs.append(pr)
        if not profs:
            raise Skip("no profiles")
        recs = [self.profile_rec(pr) for pr in profs]
        if len({r["sketch"] for r in recs}) > 1:
            raise Skip("profiles come from more than one sketch")
        return recs

    # ---- features (inputs; read with the marker just before the feature)
    OPS = {0: "join", 1: "cut", 2: "intersect", 3: "new", 4: "new"}

    def ext_param(self, ext):
        """One extent of an extrude as {mode, distance}."""
        k = kind(ext)
        if k == "DistanceExtentDefinition":
            d = adsk.fusion.DistanceExtentDefinition.cast(ext)
            return {"kind": "distance", "distance": self.param(d.distance), "symmetric": bool(d.isSymmetric)}
        if k == "ThroughAllExtentDefinition":
            t = adsk.fusion.ThroughAllExtentDefinition.cast(ext)
            return {"kind": "through_all", "positive": bool(t.isPositiveDirection)}
        if k == "AllExtentDefinition":
            a = adsk.fusion.AllExtentDefinition.cast(ext)
            return {"kind": "all", "direction": {0: "positive", 1: "negative", 2: "symmetric"}.get(a.direction, "positive")}
        if k in ("ToEntityExtentDefinition", "OneSideToExtentDefinition"):
            return {"kind": "to"}
        raise Skip("extent type %s" % k)

    def f_extrude(self, f):
        f = adsk.fusion.ExtrudeFeature.cast(f)
        rec = {"profiles": self.profiles(f), "operation": self.OPS.get(f.operation, "new")}
        try:
            if f.isThinExtrude:
                raise Skip("thin extrude")
        except Skip:
            raise
        except Exception:
            pass
        st = f.startExtent
        if st is not None and kind(st) != "ProfilePlaneStartDefinition":
            raise Skip("starts away from the sketch plane (%s)" % kind(st))
        for tp in (f.taperAngleOne, f.taperAngleTwo):
            try:
                if tp is not None and abs(tp.value) > 1e-9:
                    raise Skip("tapered")
            except Skip:
                raise
            except Exception:
                pass
        ed = f.extentDefinition
        k = kind(ed)
        if k == "SymmetricExtentDefinition":
            s = adsk.fusion.SymmetricExtentDefinition.cast(ed)
            dist = adsk.fusion.ModelParameter.cast(s.distance)
            rec["extent"] = {"mode": "symmetric", "distance": self.param(dist), "full": bool(s.isFullLength)}
        elif f.hasTwoExtents:
            rec["extent"] = {"mode": "two", "one": self.ext_param(f.extentOne), "two": self.ext_param(f.extentTwo)}
            if rec["extent"]["one"]["kind"] != "distance" or rec["extent"]["two"]["kind"] != "distance":
                raise Skip("two-sided extrude to an object")
        else:
            one = self.ext_param(f.extentOne)
            if one.get("symmetric"):
                rec["extent"] = {"mode": "symmetric", "distance": one["distance"], "full": True}
            else:
                rec["extent"] = {"mode": "one", "one": one}
        try:
            rec["targets"] = [self.body_key(b) for b in items(f.participantBodies)]
        except Exception:
            rec["targets"] = []
        return rec

    def f_extrude_after(self, f, rec):
        f = adsk.fusion.ExtrudeFeature.cast(f)
        rec["result"] = [self.body_key(b) for b in items(f.bodies)]
        # "to object" / "all" extents: measure the length that came out (end faces vs start faces)
        try:
            pr = rec["profiles"][0]
            sk_rec = self.sketch_records.get(pr["sketch"])
            if sk_rec is None:
                return
            n, o = sk_rec["z"], sk_rec["origin"]
            ends = []
            for fc in items(f.endFaces):
                frame = self.ctx_frame(fc)
                ends.append(_dot(_sub(frame.point(mm(fc.centroid)), o), n))
            if ends:
                rec["measured_end"] = ends
            starts = []
            for fc in items(f.startFaces):
                frame = self.ctx_frame(fc)
                starts.append(_dot(_sub(frame.point(mm(fc.centroid)), o), n))
            if starts:
                rec["measured_start"] = starts
        except Exception as e:
            self.log("extrude measure: %s" % e)

    def f_revolve(self, f):
        f = adsk.fusion.RevolveFeature.cast(f)
        rec = {"profiles": self.profiles(f), "operation": self.OPS.get(f.operation, "new"),
               "axis": self.axis_ref(f.axis)}
        ed = f.extentDefinition
        k = kind(ed)
        if k == "AngleExtentDefinition":
            a = adsk.fusion.AngleExtentDefinition.cast(ed)
            if a.isSymmetric:
                raise Skip("symmetric revolve")
            rec["extent"] = {"mode": "one", "angle": self.param(a.angle)}
        elif k == "TwoSidesAngleExtentDefinition":
            raise Skip("two-sided revolve")
        else:
            raise Skip("revolve extent %s" % k)
        # Fission revolves around a line in the sketch plane
        sk_rec = self.sketch_records.get(rec["profiles"][0]["sketch"])
        ax = rec["axis"]
        if sk_rec is not None and "origin" in ax:
            off = _dot(_sub(ax["origin"], sk_rec["origin"]), sk_rec["z"])
            if abs(off) > 1e-4 or abs(_dot(ax["dir"], sk_rec["z"])) > 1e-6:
                raise Skip("the axis isn't in the sketch plane")
        try:
            rec["targets"] = [self.body_key(b) for b in items(f.participantBodies)]
        except Exception:
            rec["targets"] = []
        return rec

    def f_revolve_after(self, f, rec):
        rec["result"] = [self.body_key(b) for b in items(adsk.fusion.RevolveFeature.cast(f).bodies)]

    def _edges(self, coll, tangent):
        edges = []
        seen = set()
        for x in items(coll):
            e = adsk.fusion.BRepEdge.cast(x)
            if e is None:
                fc = adsk.fusion.BRepFace.cast(x)
                if fc is not None:                     # a face means all its edges
                    for ee in items(fc.edges):
                        edges.append(ee)
                    continue
                raise Skip("edge selection contains %s" % kind(x))
            edges.append(e)
            if tangent:
                try:
                    for t in items(e.tangentiallyConnectedEdges):
                        edges.append(t)
                except Exception:
                    pass
        out = []
        for e in edges:
            try:
                key = e.tempId, self.body_key(e.body)
            except Exception:
                key = id(e)
            if key in seen:
                continue
            seen.add(key)
            out.append(self.edge_ref(e))
        return out

    def f_fillet(self, f):
        f = adsk.fusion.FilletFeature.cast(f)
        sets = []
        for s in items(f.edgeSets):
            c = adsk.fusion.ConstantRadiusFilletEdgeSet.cast(s)
            if c is None:
                raise Skip("%s fillet" % kind(s).replace("FilletEdgeSet", "").lower())
            tangent = True
            try:
                tangent = bool(c.isTangentChain)
            except Exception:
                try:
                    tangent = bool(f.isTangentChain)
                except Exception:
                    pass
            sets.append({"radius": self.param(c.radius), "edges": self._edges(c.edges, tangent)})
        if not sets:
            raise Skip("no edge sets")
        return {"sets": sets}

    def f_chamfer(self, f):
        f = adsk.fusion.ChamferFeature.cast(f)
        sets = []
        try:
            es = items(f.edgeSets)
        except Exception:
            es = []
        for s in es:
            c = adsk.fusion.EqualDistanceChamferEdgeSet.cast(s)
            if c is None:
                raise Skip("%s chamfer" % kind(s).replace("ChamferEdgeSet", ""))
            tangent = True
            try:
                tangent = bool(c.isTangentChain)
            except Exception:
                pass
            sets.append({"distance": self.param(c.distance), "edges": self._edges(c.edges, tangent)})
        if not sets:                                # older API: one definition for the whole feature
            if f.chamferType != 0:
                raise Skip("two-distance / angle chamfer")
            d = adsk.fusion.EqualDistanceChamferTypeDefinition.cast(f.chamferTypeDefinition)
            sets.append({"distance": self.param(d.distance), "edges": self._edges(f.edges, bool(f.isTangentChain))})
        return {"sets": sets}

    def f_shell(self, f):
        f = adsk.fusion.ShellFeature.cast(f)
        faces, bodies = [], []
        tangent = False
        try:
            tangent = bool(f.isTangentChain)
        except Exception:
            pass
        for x in items(f.inputEntities):
            fc = adsk.fusion.BRepFace.cast(x)
            if fc is not None:
                faces.append(self.face_ref(fc))
                if tangent:
                    for t in items(fc.tangentiallyConnectedFaces):
                        faces.append(self.face_ref(t))
                continue
            b = adsk.fusion.BRepBody.cast(x)
            if b is not None:
                bodies.append(self.body_key(b))
                continue
            raise Skip("shell input %s" % kind(x))
        inside, outside = f.insideThickness, f.outsideThickness
        iv = inside.value if inside is not None else 0
        ov = outside.value if outside is not None else 0
        if iv > 1e-9 and ov > 1e-9:
            raise Skip("shell with both inside and outside thickness")
        rec = {"faces": faces, "bodies": bodies}
        if ov > 1e-9:
            rec.update(direction="outside", thickness=self.param(outside))
        else:
            rec.update(direction="inside", thickness=self.param(inside))
        return rec

    def _inputs(self, coll):
        """Bodies or features a mirror / pattern copies."""
        bodies, feats = [], []
        for x in items(coll):
            b = adsk.fusion.BRepBody.cast(x)
            if b is not None:
                bodies.append(self.body_key(b))
                continue
            fe = adsk.fusion.Feature.cast(x)
            if fe is not None:
                feats.append(fe.timelineObject.index)
                continue
            raise Skip("copies %s (only bodies and features can be rebuilt)" % kind(x))
        if bodies and feats:
            raise Skip("mixes bodies and features")
        return {"bodies": bodies} if bodies else {"features": feats}

    def f_mirror(self, f):
        f = adsk.fusion.MirrorFeature.cast(f)
        rec = self._inputs(f.inputEntities)
        rec["plane"] = self.plane_ref(f.mirrorPlane)
        try:
            rec["combine"] = bool(f.isCombine)
        except Exception:
            rec["combine"] = False
        return rec

    def f_mirror_after(self, f, rec):
        rec["result"] = [self.body_key(b) for b in items(f.bodies)]

    def f_cpattern(self, f):
        f = adsk.fusion.CircularPatternFeature.cast(f)
        rec = self._inputs(f.inputEntities)
        rec["axis"] = self.axis_ref(f.axis)
        if f.isSymmetric:
            raise Skip("symmetric circular pattern")
        try:
            if list(f.suppressedElementsIds):
                raise Skip("pattern with suppressed copies")
        except Skip:
            raise
        except Exception:
            pass
        rec["count"] = self.param(f.quantity)
        rec["angle"] = self.param(f.totalAngle)
        return rec

    def f_rpattern(self, f):
        f = adsk.fusion.RectangularPatternFeature.cast(f)
        rec = self._inputs(f.inputEntities)
        if f.isSymmetricInDirectionOne or (f.directionTwoEntity is not None and f.isSymmetricInDirectionTwo):
            raise Skip("symmetric rectangular pattern")
        try:
            if list(f.suppressedElementsIds):
                raise Skip("pattern with suppressed copies")
        except Skip:
            raise
        except Exception:
            pass
        rec["spacing"] = "extent" if f.patternDistanceType == 0 else "spacing"
        frame = self.frame_of(f.parentComponent)
        rec["dir1"] = frame.dir(vec(f.directionOne))
        rec["count1"] = self.param(f.quantityOne)
        rec["distance1"] = self.param(f.distanceOne)
        if f.directionTwoEntity is not None:
            rec["dir2"] = frame.dir(vec(f.directionTwo))
            rec["count2"] = self.param(f.quantityTwo)
            rec["distance2"] = self.param(f.distanceTwo)
        try:
            e1 = adsk.fusion.BRepEdge.cast(f.directionOneEntity)
            if e1 is not None:
                rec["dir1_edge"] = self.edge_ref(e1)
            e2 = adsk.fusion.BRepEdge.cast(f.directionTwoEntity)
            if e2 is not None:
                rec["dir2_edge"] = self.edge_ref(e2)
        except Exception:
            pass
        return rec

    def f_combine(self, f):
        f = adsk.fusion.CombineFeature.cast(f)
        return {"target": self.body_key(f.targetBody),
                "tools": [self.body_key(b) for b in items(f.toolBodies)],
                "operation": self.OPS.get(f.operation, "join"), "keep": bool(f.isKeepToolBodies),
                "new_component": bool(f.isNewComponent)}

    def f_move(self, f):
        f = adsk.fusion.MoveFeature.cast(f)
        bodies = []
        for x in items(f.inputEntities):
            b = adsk.fusion.BRepBody.cast(x)
            if b is None:
                raise Skip("moves %s (only bodies can be rebuilt)" % kind(x))
            bodies.append(self.body_key(b))
        m = f.transform
        o, x, y, z = m.getAsCoordinateSystem()
        mv = Frame(mm(o), vec(x), vec(y), vec(z))
        occ = self.frame_of(f.parentComponent)
        if not occ.is_identity():
            # world move = occurrence * move * occurrence^-1
            def world(p):
                local = [_dot(_sub(p, occ.o), occ.x), _dot(_sub(p, occ.o), occ.y), _dot(_sub(p, occ.o), occ.z)]
                return occ.point(mv.point(local))
            o2 = world([0, 0, 0])
            ax = [_sub(world(e), o2) for e in ([1, 0, 0], [0, 1, 0], [0, 0, 1])]
            mv = Frame(o2, ax[0], ax[1], ax[2])
        return {"bodies": bodies, "matrix": {"origin": mv.o, "x": mv.x, "y": mv.y, "z": mv.z}}

    def f_cplane(self, cp):
        cp = adsk.fusion.ConstructionPlane.cast(cp)
        frame = self.ctx_frame(cp)
        g = cp.geometry
        rec = {"origin": frame.point(mm(g.origin)), "normal": frame.dir(vec(g.normal)),
               "u": frame.dir(vec(g.uDirection)), "name": cp.name}
        d = adsk.fusion.ConstructionPlaneOffsetDefinition.cast(cp.definition)
        if d is not None:
            try:
                rec["base"] = self.plane_ref(d.planarEntity)
                rec["offset"] = self.param(adsk.fusion.ModelParameter.cast(d.offset))
            except Skip as e:
                rec["why"] = str(e)
        else:
            rec["definition"] = kind(cp.definition)
        return rec

    HANDLERS = {
        "ExtrudeFeature": ("extrude", f_extrude, f_extrude_after),
        "RevolveFeature": ("revolve", f_revolve, f_revolve_after),
        "FilletFeature": ("fillet", f_fillet, None),
        "ChamferFeature": ("chamfer", f_chamfer, None),
        "ShellFeature": ("shell", f_shell, None),
        "MirrorFeature": ("mirror", f_mirror, f_mirror_after),
        "CircularPatternFeature": ("cpattern", f_cpattern, f_mirror_after),
        "RectangularPatternFeature": ("rpattern", f_rpattern, f_mirror_after),
        "CombineFeature": ("combine", f_combine, None),
        "MoveFeature": ("move", f_move, None),
    }
    # ---- snapshots
    def snapshot(self, name):
        """STEP of the whole design as it is right now (all bodies switched on for the moment)."""
        path = os.path.join(self.workdir, name)
        flipped = []
        try:
            for occ in items(self.root.allOccurrences):
                try:
                    if not occ.isLightBulbOn:
                        occ.isLightBulbOn = True
                        flipped.append(occ)
                except Exception:
                    pass
            for _, b in self.all_bodies():
                try:
                    if b.isSolid and not b.isLightBulbOn:
                        b.isLightBulbOn = True
                        flipped.append(b)
                except Exception:
                    pass
            try:
                if not self.root.isBodiesFolderLightBulbOn:
                    self.root.isBodiesFolderLightBulbOn = True
                    flipped.append("folder")
            except Exception:
                pass
            em = self.design.exportManager
            opts = em.createSTEPExportOptions(path, self.root)
            ok = em.execute(opts)
            if not ok or not os.path.exists(path):
                raise RuntimeError("STEP export returned no file")
            self.snapshots += 1
            return name
        except Exception as e:
            self.log("snapshot %s failed: %s" % (name, e))
            return None
        finally:
            for x in flipped:
                try:
                    if x == "folder":
                        self.root.isBodiesFolderLightBulbOn = False
                    else:
                        x.isLightBulbOn = False
                except Exception:
                    pass

    # ---- the walk
    def timeline_items(self, tl):
        """Every timeline object in order (groups opened up)."""
        found = {}

        def walk(coll):
            for it in items(coll):
                try:
                    if it.isGroup:
                        walk(adsk.fusion.TimelineGroup.cast(it))
                        continue
                    found[it.index] = it
                except Exception:
                    pass
        walk(tl)
        return [found[k] for k in sorted(found) if k >= 0]

    def run(self, out_path, progress=None):
        design = self.design
        info = {"name": "", "fusion": "", "parametric": True, "units": ""}
        try:
            info["name"] = self.app.activeDocument.name
        except Exception:
            pass
        try:
            info["fusion"] = self.app.version
        except Exception:
            pass
        try:
            info["units"] = design.unitsManager.defaultLengthUnits
        except Exception:
            pass
        try:
            pref = self.app.preferences.generalPreferences.defaultModelingOrientation
            info["up_preference"] = "y" if pref == 0 else "z"
        except Exception:
            pass
        info["parametric"] = design.designType == adsk.fusion.DesignTypes.ParametricDesignType

        timeline = []
        self.sketch_records = {}
        self._sketch_ids = {}
        tl = None
        start_marker = None
        if info["parametric"]:
            tl = design.timeline
            start_marker = tl.markerPosition
        try:
            objs = self.timeline_items(tl) if tl is not None else []
            if progress:
                progress.show("Fission Export", "Reading the timeline: %v of %m", 0, max(1, len(objs) + 1), 0)
            prev_stats = None
            pending = []                  # unsupported, body-changing items waiting for a snapshot
            if objs:
                try:
                    tl.moveToBeginning()
                    prev_stats = self.stats()
                except Exception as e:
                    self.log("moveToBeginning: %s" % e)
            first_stats = prev_stats
            for n, obj in enumerate(objs):
                if progress:
                    if progress.wasCancelled:
                        self.cancelled = True
                        break
                    progress.progressValue = n
                rec = {"i": obj.index, "name": obj.name}
                ent = None
                try:
                    ent = obj.entity
                    rec["type"] = kind(ent) if ent is not None else "?"
                except Exception:
                    rec["type"] = "?"
                try:
                    rec["suppressed"] = bool(obj.isSuppressed)
                except Exception:
                    rec["suppressed"] = False
                typ = rec["type"]
                handler = self.HANDLERS.get(typ)

                # 1) marker just before the item: inputs
                try:
                    obj.rollTo(True)
                except Exception as e:
                    self.log("rollTo(before) %s: %s" % (obj.name, e))
                # anything that may build on the unsupported geometry gets a snapshot in front of it
                if pending and not rec["suppressed"] and (handler is not None or typ in ("Sketch", "ConstructionPlane")):
                    snap = self.snapshot("snap_%d.step" % obj.index)
                    for p in pending:
                        p["snapshot_at"] = obj.index
                    timeline.append({"i": obj.index - 0.5, "type": "_snapshot", "name": "snapshot",
                                     "snapshot": snap, "after": self.stats(), "covers": [p["i"] for p in pending]})
                    pending = []
                if not rec["suppressed"] and ent is not None:
                    try:
                        if handler is not None:
                            rec["kind"] = handler[0]
                            rec["data"] = handler[1](self, ent)
                        elif typ == "ConstructionPlane":
                            rec["kind"] = "cplane"
                    except Skip as e:
                        rec["unsupported"] = str(e)
                    except Exception as e:
                        rec["unsupported"] = "couldn't read it (%s)" % e
                        self.log("read %s failed:\n%s" % (obj.name, traceback.format_exc()))

                # 2) marker just after: results
                try:
                    obj.rollTo(False)
                except Exception as e:
                    self.log("rollTo(after) %s: %s" % (obj.name, e))
                if not rec["suppressed"] and ent is not None:
                    try:
                        if typ == "Sketch":
                            rec["kind"] = "sketch"
                            rec["data"] = self.sketch(adsk.fusion.Sketch.cast(ent))
                            self.sketch_records[obj.index] = rec["data"]
                        elif typ == "ConstructionPlane":
                            rec["data"] = self.f_cplane(ent)
                        elif handler is not None and handler[2] is not None and "data" in rec:
                            handler[2](self, ent, rec["data"])
                    except Skip as e:
                        rec["unsupported"] = str(e)
                    except Exception as e:
                        rec["unsupported"] = "couldn't read it (%s)" % e
                        self.log("read %s failed:\n%s" % (obj.name, traceback.format_exc()))
                    try:
                        hs = obj.healthState
                        if hs in (1, 2):
                            rec["health"] = "warning" if hs == 1 else "error"
                            rec["message"] = obj.errorOrWarningMessage
                    except Exception:
                        pass
                rec["after"] = self.stats()
                changed = not _same_stats(prev_stats, rec["after"])
                rec["changed"] = changed
                if "kind" not in rec and not rec["suppressed"] and changed:
                    rec["unsupported"] = rec.get("unsupported") or ("%s isn't in Fission yet" % typ)
                if rec.get("unsupported") and changed:
                    pending.append(rec)
                prev_stats = rec["after"]
                timeline.append(rec)
            # 3) back to the end for the final shape
            if tl is not None:
                try:
                    tl.moveToEnd()
                except Exception:
                    pass
            # the finished part is always saved: Fission uses it to check (and if needed correct) its rebuild
            final = {"after": self.stats(), "snapshot": self.snapshot("final.step")}
            for p in pending:
                p["snapshot_at"] = "final"
        finally:
            if tl is not None and start_marker is not None:
                try:
                    tl.markerPosition = start_marker
                except Exception:
                    pass

        doc = {"format": FORMAT, "version": VERSION, "design": info,
               "params": self.all_params(), "start": first_stats, "timeline": timeline, "final": final,
               "exported": time.strftime("%Y-%m-%d %H:%M:%S"), "cancelled": self.cancelled}
        with open(os.path.join(self.workdir, "timeline.json"), "w", encoding="utf-8") as fh:
            json.dump(doc, fh)
        with open(os.path.join(self.workdir, "log.txt"), "w", encoding="utf-8") as fh:
            fh.write("\n".join(self.lines))
        with zipfile.ZipFile(out_path, "w", zipfile.ZIP_DEFLATED) as z:
            for name in os.listdir(self.workdir):
                z.write(os.path.join(self.workdir, name), name)
        shutil.rmtree(self.workdir, ignore_errors=True)
        return doc


def _same_stats(a, b):
    if a is None or b is None:
        return a == b
    if len(a) != len(b):
        return False
    ka = {r["key"]: r for r in a}
    for r in b:
        o = ka.get(r["key"])
        if o is None:
            return False
        if abs(o["volume"] - r["volume"]) > 1e-6 * max(1.0, abs(r["volume"])):
            return False
        if abs(o["area"] - r["area"]) > 1e-6 * max(1.0, abs(r["area"])):
            return False
        for i in range(2):
            for j in range(3):
                if abs(o["bbox"][i][j] - r["bbox"][i][j]) > 1e-6:
                    return False
    return True


def _summary(doc):
    tl = [r for r in doc["timeline"] if r.get("type") != "_snapshot"]
    unsupported = [r for r in tl if r.get("unsupported") and r.get("changed")]
    lines = ["Saved %d timeline items." % len(tl)]
    if unsupported:
        names = ", ".join(r["name"] for r in unsupported[:8]) + (" ..." if len(unsupported) > 8 else "")
        lines.append("%d can't be rebuilt in Fission yet and come in as frozen shapes: %s" % (len(unsupported), names))
    if not doc["design"].get("parametric", True):
        lines.append("This design doesn't keep history (direct modelling), so only the bodies come across.")
    if doc.get("cancelled"):
        lines.append("You cancelled part way: the rest of the timeline comes in as one frozen shape.")
    lines.append("\nOpen the .ffx file in Fission with MANAGE > ADD-INS > Open from Fusion.")
    return "\n".join(lines)


def run(context):
    app = adsk.core.Application.get()
    ui = app.userInterface
    try:
        design = adsk.fusion.Design.cast(app.activeProduct)
        if design is None:
            ui.messageBox("Open a design (not a drawing or CAM setup) and run Fission Export again.",
                          "Fission Export")
            return
        dlg = ui.createFileDialog()
        dlg.title = "Save for Fission"
        dlg.filter = "Fission export (*.ffx)"
        base = "design"
        try:
            base = "".join(ch for ch in app.activeDocument.name if ch not in '<>:"/\\|?*').strip() or "design"
        except Exception:
            pass
        dlg.initialFilename = base + ".ffx"
        if dlg.showSave() != adsk.core.DialogResults.DialogOK:
            return
        path = dlg.filename
        if not path.lower().endswith(".ffx"):
            path += ".ffx"
        progress = ui.createProgressDialog()
        progress.isCancelButtonShown = True
        exp = Exporter(app, design, ui)
        try:
            doc = exp.run(path, progress)
        finally:
            try:
                progress.hide()
            except Exception:
                pass
        ui.messageBox(_summary(doc) + "\n\nSaved to:\n" + path, "Fission Export")
    except Exception:
        if ui:
            ui.messageBox("Fission Export failed:\n\n" + traceback.format_exc(), "Fission Export")
