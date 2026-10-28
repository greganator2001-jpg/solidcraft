"""Fission's stand-in for Fusion 360's API: the classes a Fusion script uses, each turning into Fission
commands (through fission_bridge.call). Fusion's units are kept on this side: lengths in centimetres,
angles in radians; Fission works in millimetres and degrees.

Sketches are drawn on the same axes Fusion uses (its XZ sketch has y pointing to -Z), so a script's
coordinates land where they would in Fusion.
"""
import math

from fission_bridge import call, message, FissionError  # noqa: F401

CM = 10.0  # Fusion's internal centimetres -> Fission's millimetres


def mm(v):
    return float(v) * CM


def _cm(v):
    return float(v) / CM


class NotSupported(Exception):
    """A part of Fusion's API that Fission doesn't have yet."""


class Base:
    """Every object: `cast` gives the object back, `isValid` is True, unknown attributes say so plainly."""

    @classmethod
    def cast(cls, obj):
        return obj

    @property
    def isValid(self):
        return True

    @property
    def objectType(self):
        return "adsk::fusion::" + type(self).__name__


# ---------------------------------------------------------------- geometry values
class Point3D(Base):
    def __init__(self, x=0.0, y=0.0, z=0.0):
        self.x, self.y, self.z = float(x), float(y), float(z)

    @staticmethod
    def create(x=0.0, y=0.0, z=0.0):
        return Point3D(x, y, z)

    def copy(self):
        return Point3D(self.x, self.y, self.z)

    def asArray(self):
        return [self.x, self.y, self.z]

    def distanceTo(self, o):
        return math.dist(self.asArray(), o.asArray())

    def translateBy(self, v):
        self.x += v.x
        self.y += v.y
        self.z += v.z
        return True

    def vectorTo(self, o):
        return Vector3D(o.x - self.x, o.y - self.y, o.z - self.z)

    def isEqualTo(self, o):
        return self.distanceTo(o) < 1e-9

    def transformBy(self, m):
        x, y, z = m.apply(self.asArray())
        self.x, self.y, self.z = x, y, z
        return True

    def setWithArray(self, a):
        self.x, self.y, self.z = (float(v) for v in a[:3])
        return True

    def __repr__(self):
        return "Point3D({:.4g}, {:.4g}, {:.4g})".format(self.x, self.y, self.z)


class Point2D(Base):
    def __init__(self, x=0.0, y=0.0):
        self.x, self.y = float(x), float(y)

    @staticmethod
    def create(x=0.0, y=0.0):
        return Point2D(x, y)


class Vector3D(Base):
    def __init__(self, x=0.0, y=0.0, z=0.0):
        self.x, self.y, self.z = float(x), float(y), float(z)

    @staticmethod
    def create(x=0.0, y=0.0, z=0.0):
        return Vector3D(x, y, z)

    def asArray(self):
        return [self.x, self.y, self.z]

    @property
    def length(self):
        return math.sqrt(self.x ** 2 + self.y ** 2 + self.z ** 2)

    def normalize(self):
        n = self.length or 1.0
        self.x, self.y, self.z = self.x / n, self.y / n, self.z / n
        return True

    def scaleBy(self, s):
        self.x, self.y, self.z = self.x * s, self.y * s, self.z * s
        return True

    def add(self, v):
        self.x, self.y, self.z = self.x + v.x, self.y + v.y, self.z + v.z
        return True

    def subtract(self, v):
        self.x, self.y, self.z = self.x - v.x, self.y - v.y, self.z - v.z
        return True

    def dotProduct(self, v):
        return self.x * v.x + self.y * v.y + self.z * v.z

    def crossProduct(self, v):
        return Vector3D(self.y * v.z - self.z * v.y, self.z * v.x - self.x * v.z, self.x * v.y - self.y * v.x)

    def angleTo(self, v):
        d = self.length * v.length
        return math.acos(max(-1.0, min(1.0, self.dotProduct(v) / d))) if d else 0.0

    def copy(self):
        return Vector3D(self.x, self.y, self.z)

    def asPoint(self):
        return Point3D(self.x, self.y, self.z)


class Matrix3D(Base):
    """A rigid 4x4 transform (rows), translation in centimetres."""

    def __init__(self):
        self.m = [[1.0 if i == j else 0.0 for j in range(4)] for i in range(4)]

    @staticmethod
    def create():
        return Matrix3D()

    def setToIdentity(self):
        self.__init__()
        return True

    def apply(self, p):
        return [sum(self.m[i][k] * p[k] for k in range(3)) + self.m[i][3] for i in range(3)]

    @property
    def translation(self):
        return Vector3D(self.m[0][3], self.m[1][3], self.m[2][3])

    @translation.setter
    def translation(self, v):
        self.m[0][3], self.m[1][3], self.m[2][3] = v.x, v.y, v.z

    def setToRotation(self, angle, axis, origin):
        a = axis.asArray()
        n = math.sqrt(sum(c * c for c in a)) or 1.0
        x, y, z = (c / n for c in a)
        c, s, t = math.cos(angle), math.sin(angle), 1 - math.cos(angle)
        r = [[t * x * x + c, t * x * y - s * z, t * x * z + s * y],
             [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
             [t * x * z - s * y, t * y * z + s * x, t * z * z + c]]
        o = origin.asArray()
        ro = [sum(r[i][k] * o[k] for k in range(3)) for i in range(3)]
        for i in range(3):
            self.m[i][:3] = r[i]
            self.m[i][3] = o[i] - ro[i]
        return True

    def transformBy(self, other):
        a, b = other.m, self.m
        self.m = [[sum(a[i][k] * b[k][j] for k in range(4)) for j in range(4)] for i in range(4)]
        return True

    def invert(self):
        r = [[self.m[j][i] for j in range(3)] for i in range(3)]
        t = [self.m[i][3] for i in range(3)]
        rt = [-sum(r[i][k] * t[k] for k in range(3)) for i in range(3)]
        for i in range(3):
            self.m[i][:3] = r[i]
            self.m[i][3] = rt[i]
        return True

    def copy(self):
        o = Matrix3D()
        o.m = [row[:] for row in self.m]
        return o

    def axis_angle(self):
        """(axis, degrees) of the rotation part."""
        r = self.m
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
        axis = [(r[2][1] - r[1][2]) / s, (r[0][2] - r[2][0]) / s, (r[1][0] - r[0][1]) / s]
        return axis, math.degrees(ang)


class BoundingBox3D(Base):
    def __init__(self, lo=None, hi=None):
        self.minPoint = lo or Point3D()
        self.maxPoint = hi or Point3D()

    @staticmethod
    def create(lo, hi):
        return BoundingBox3D(lo, hi)

    def contains(self, p):
        return all(a - 1e-9 <= b <= c + 1e-9 for a, b, c in zip(self.minPoint.asArray(), p.asArray(), self.maxPoint.asArray()))


class Line3D(Base):
    curveType = 0

    def __init__(self, a, b):
        self.startPoint, self.endPoint = a, b


class InfiniteLine3D(Base):
    def __init__(self, origin, direction):
        self.origin, self.direction = origin, direction


class Circle3D(Base):
    curveType = 2

    def __init__(self, center, normal, radius):
        self.center, self.normal, self.radius = center, normal, radius


class Arc3D(Circle3D):
    curveType = 1


class Plane(Base):
    surfaceType = 0

    def __init__(self, origin, normal):
        self.origin, self.normal = origin, normal

    @staticmethod
    def create(origin, normal):
        return Plane(origin, normal)


class Cylinder(Base):
    surfaceType = 1

    def __init__(self, origin, axis, radius):
        self.origin, self.axis, self.radius = origin, axis, radius


class SurfaceTypes:
    PlaneSurfaceType, CylinderSurfaceType, ConeSurfaceType, SphereSurfaceType, TorusSurfaceType, NurbsSurfaceType = range(6)


class Curve3DTypes:
    Line3DCurveType, Arc3DCurveType, Circle3DCurveType, Ellipse3DCurveType, EllipticalArc3DCurveType, NurbsCurve3DCurveType = range(6)


class ObjectCollection(Base):
    def __init__(self):
        self._items = []

    @staticmethod
    def create():
        return ObjectCollection()

    @staticmethod
    def createWithArray(items):
        c = ObjectCollection()
        c._items = list(items)
        return c

    def add(self, item):
        self._items.append(item)
        return True

    def removeByIndex(self, i):
        del self._items[i]
        return True

    def removeByItem(self, item):
        self._items.remove(item)
        return True

    def clear(self):
        self._items = []
        return True

    def contains(self, item):
        return item in self._items

    def find(self, item, start=0):
        try:
            return self._items.index(item, start)
        except ValueError:
            return -1

    def item(self, i):
        return self._items[i]

    @property
    def count(self):
        return len(self._items)

    def __iter__(self):
        return iter(list(self._items))

    def __len__(self):
        return len(self._items)

    def __getitem__(self, i):
        return self._items[i]


def items(obj):
    """A collection, a list or one object, as a list."""
    if obj is None:
        return []
    if isinstance(obj, (list, tuple)):
        return list(obj)
    if isinstance(obj, ObjectCollection):
        return list(obj._items)
    return [obj]


class ValueInput(Base):
    """A value: a real in Fusion's units (cm, radians) or an expression text ('5 mm', 'thick')."""

    def __init__(self, real=None, text=None, obj=None):
        self.realValue = real
        self.stringValue = text
        self.objectValue = obj

    @staticmethod
    def createByReal(v):
        return ValueInput(real=float(v))

    @staticmethod
    def createByString(s):
        return ValueInput(text=str(s))

    @staticmethod
    def createByObject(o):
        return ValueInput(obj=o)

    @property
    def valueType(self):
        return 0 if self.realValue is not None else 1

    def length(self):
        """For Fission: a length in mm, or the expression."""
        if self.realValue is not None:
            return round(mm(self.realValue), 9)
        return self.stringValue

    def angle(self):
        if self.realValue is not None:
            return "{} deg".format(round(math.degrees(self.realValue), 9))
        return self.stringValue

    def number(self):
        if self.realValue is not None:
            return self.realValue
        try:
            return float(self.stringValue)
        except (TypeError, ValueError):
            return self.stringValue


def length_of(v):
    return v.length() if isinstance(v, ValueInput) else mm(v)


def angle_of(v):
    return v.angle() if isinstance(v, ValueInput) else "{} deg".format(math.degrees(float(v)))


# ---------------------------------------------------------------- the application
class DialogResults:
    DialogError, DialogOK, DialogCancel, DialogYes, DialogNo = -1, 0, 1, 2, 3


class MessageBoxButtonTypes:
    OKButtonType, OKCancelButtonType, RetryCancelButtonType, YesNoButtonType, YesNoCancelButtonType = range(5)


class MessageBoxIconTypes:
    NoIconIconType, QuestionIconType, InformationIconType, WarningIconType, CriticalIconType = range(5)


class DocumentTypes:
    FusionDesignDocumentType, DrawingDocumentType = 0, 1


class HorizontalAlignments:
    LeftHorizontalAlignment, CenterHorizontalAlignment, RightHorizontalAlignment = range(3)


class VerticalAlignments:
    TopVerticalAlignment, MiddleVerticalAlignment, BottomVerticalAlignment = range(3)


class UserInterface(Base):
    def messageBox(self, text, title="", buttons=0, icon=0):
        text = str(text)
        if "Traceback (most recent call last)" in text:
            # the script caught its own error and showed it: that's a failure
            raise RuntimeError(text.strip().splitlines()[-1])
        message(text)
        return DialogResults.DialogYes if buttons in (3, 4) else DialogResults.DialogOK

    def inputBox(self, prompt, title="", default=""):
        # no one to ask: the default, as if OK was pressed
        return [default, False]

    @property
    def activeSelections(self):
        return ObjectCollection()


class Viewport(Base):
    def fit(self):
        return True

    def refresh(self):
        return True


class Document(Base):
    def __init__(self, name="Untitled"):
        self.name = name

    def close(self, save=False):
        return True

    @property
    def design(self):
        return _design()


class Documents(Base):
    def add(self, kind=0, visible=True, options=None):
        call("file.new", {})
        _reset()
        return Document()

    @property
    def count(self):
        return 1


class Application(Base):
    _app = None

    @staticmethod
    def get():
        if Application._app is None:
            Application._app = Application()
        return Application._app

    @property
    def userInterface(self):
        return UserInterface()

    @property
    def activeProduct(self):
        return _design()

    @property
    def activeDocument(self):
        return Document()

    @property
    def documents(self):
        return Documents()

    @property
    def activeViewport(self):
        return Viewport()

    def log(self, text, *a):
        print(text)


def terminate():
    return True


def autoTerminate(flag=True):
    return True


def doEvents():
    return True


# ---------------------------------------------------------------- the design
class FeatureOperations:
    JoinFeatureOperation, CutFeatureOperation, IntersectFeatureOperation, NewBodyFeatureOperation, NewComponentFeatureOperation = range(5)


_OPS = {0: "join", 1: "cut", 2: "intersect", 3: "new", 4: "new"}


class ExtentDirections:
    PositiveExtentDirection, NegativeExtentDirection, SymmetricExtentDirection = range(3)


class DesignTypes:
    DirectDesignType, ParametricDesignType = 0, 1


class PatternDistanceType:
    ExtentPatternDistanceType, SpacingPatternDistanceType = 0, 1


class ChamferTypes:
    EqualDistanceChamferType, TwoDistancesChamferType, DistanceAndAngleChamferType = range(3)


class TextStyles:
    TextStyleBold, TextStyleItalic, TextStyleUnderline = 1, 2, 4


class Lst(Base):
    def __init__(self, things=None):
        self._items = list(things or [])

    def item(self, i):
        return self._items[i]

    @property
    def count(self):
        return len(self._items)

    def __iter__(self):
        return iter(list(self._items))

    def __len__(self):
        return len(self._items)

    def __getitem__(self, i):
        return self._items[i]

    def itemByName(self, name):
        return next((x for x in self._items if getattr(x, "name", None) == name), None)


_STATE = {}


def _reset():
    _STATE.clear()


def _design():
    d = _STATE.get("design")
    if d is None:
        d = _STATE["design"] = Design()
    return d


# --- sketch activity: Fission draws into the active sketch; features need none active
def _activate(sketch):
    cur = _STATE.get("active")
    if cur is sketch:
        return
    if cur is not None:
        call("sketch.finish", {})
    call("sketch.edit", {"sketch": sketch.id})
    _STATE["active"] = sketch


def _leave_sketch():
    if _STATE.get("active") is not None:
        call("sketch.finish", {})
        _STATE["active"] = None


class UserParameter(Base):
    def __init__(self, name, expression, unit, comment=""):
        self.name, self._expr, self.unit, self.comment = name, expression, unit, comment

    @property
    def expression(self):
        return self._expr

    @expression.setter
    def expression(self, e):
        call("parameters.change", {"name": self.name, "expression": str(e)})
        self._expr = str(e)

    @property
    def value(self):
        r = call("parameters.change", {"name": self.name, "expression": self._expr}) or {}
        v = (r.get("changed") or {}).get(self.name)
        return _cm(v) if isinstance(v, (int, float)) and self.unit in ("mm", "cm", "m", "in", "ft") else v

    @value.setter
    def value(self, v):
        self.expression = "{} mm".format(mm(v)) if self.unit in ("mm", "cm", "m", "in", "ft") else str(v)

    def deleteMe(self):
        call("parameters.change", {"delete": self.name})
        return True


class UserParameters(Lst):
    def add(self, name, value, units="mm", comment=""):
        if isinstance(value, ValueInput) and value.realValue is not None:
            if units in ("mm", "cm", "m", "in", "ft"):
                expr = "{} mm".format(mm(value.realValue))
            elif units in ("deg", "rad"):
                expr = "{} deg".format(math.degrees(value.realValue))
            else:
                expr = str(value.realValue)
        else:
            expr = value.stringValue if isinstance(value, ValueInput) else str(value)
        params = {"name": name, "expression": expr, "comment": comment}
        if units:
            params["unit"] = units
        call("parameters.add", params)
        p = UserParameter(name, expr, units, comment)
        self._items.append(p)
        return p


class Design(Base):
    def __init__(self):
        self.designType = DesignTypes.ParametricDesignType
        self.userParameters = UserParameters()
        self.rootComponent = Component()
        self.allParameters = self.userParameters

    @property
    def unitsManager(self):
        return UnitsManager()

    @property
    def timeline(self):
        return Lst()

    @property
    def activeComponent(self):
        return self.rootComponent

    def exportManager(self):
        raise NotSupported("exportManager: use Fission's File > Export")


class UnitsManager(Base):
    defaultLengthUnits = "mm"
    distanceDisplayUnits = 0

    def evaluateExpression(self, expr, units="mm"):
        r = call("parameters.add", {"name": "_fission_eval", "expression": str(expr), "unit": units})
        call("parameters.change", {"delete": "_fission_eval"})
        v = (r or {}).get("value")
        return _cm(v) if isinstance(v, (int, float)) and units in ("mm", "cm", "m", "in", "ft") else v

    def convert(self, value, frm, to):
        f = {"mm": 0.1, "cm": 1.0, "m": 100.0, "in": 2.54, "ft": 30.48}
        return value * f.get(frm, 1.0) / f.get(to, 1.0)


# ---------------------------------------------------------------- construction geometry
# Fusion's sketch axes on its origin planes (sketch x, sketch y), in world directions
_STD = {"XY": ([1, 0, 0], [0, 1, 0]), "XZ": ([1, 0, 0], [0, 0, -1]), "YZ": ([0, 0, -1], [0, 1, 0])}


def _cross(a, b):
    return [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]


class ConstructionPlane(Base):
    """An origin plane or a construction plane: its frame in mm (origin, x, y) and its Fission name."""

    def __init__(self, name, origin=(0, 0, 0), x=(1, 0, 0), y=(0, 1, 0), fission=None):
        self.name = name
        self.origin, self.x, self.y = list(origin), list(x), list(y)
        self.fission = fission or name
        self.isLightBulbOn = True
        self.isVisible = True

    @property
    def normal(self):
        return _cross(self.x, self.y)

    @property
    def geometry(self):
        return Plane(Point3D(*(_cm(v) for v in self.origin)), Vector3D(*self.normal))

    def deleteMe(self):
        return True


def _std_plane(name):
    x, y = _STD[name]
    return ConstructionPlane(name, (0, 0, 0), x, y, fission=name)


class ConstructionAxis(Base):
    def __init__(self, name, direction):
        self.name, self.direction = name, direction

    @property
    def geometry(self):
        return InfiniteLine3D(Point3D(), Vector3D(*self.direction))


class ConstructionPoint(Base):
    def __init__(self, p=(0, 0, 0)):
        self.geometry = Point3D(*p)


class ConstructionPlaneInput(Base):
    def __init__(self):
        self._how = None

    def setByOffset(self, plane, offset):
        self._how = ("offset", plane, offset)
        return True

    def setByAngle(self, line, angle, plane):
        raise NotSupported("construction plane at an angle")


class ConstructionPlanes(Lst):
    def createInput(self, occurrence=None):
        return ConstructionPlaneInput()

    def add(self, inp):
        how = inp._how
        if not how or how[0] != "offset":
            raise NotSupported("only offset construction planes so far")
        _leave_sketch()
        _, base, off = how
        d = length_of(off) if isinstance(off, ValueInput) else mm(off)
        if not isinstance(d, (int, float)):
            raise NotSupported("an offset plane needs a number (not '{}')".format(d))
        if isinstance(base, BRepFace):
            base_spec = {"face": base._point_mm()}
            n = base._normal()
            origin = [p + c * d for p, c in zip(base._point_mm(), n)]
            x = [1, 0, 0] if abs(n[0]) < 0.9 else [0, 1, 0]
            y = _cross(n, x)
            x = _cross(y, n)
        else:
            base_spec = base.fission
            n = base.normal
            origin = [o + c * d for o, c in zip(base.origin, n)]
            x, y = base.x, base.y
        r = call("construct.plane.offset", {"base": base_spec, "offset": d}) or {}
        pl = ConstructionPlane(r.get("name", "Plane"), origin, x, y, fission=r.get("name"))
        self._items.append(pl)
        return pl


# ---------------------------------------------------------------- sketches
class SketchPoint(Base):
    def __init__(self, sketch, pid, at):
        self.sketch, self.id, self._at = sketch, pid, at  # at: [x, y] mm in the sketch

    @property
    def geometry(self):
        return Point3D(_cm(self._at[0]), _cm(self._at[1]), 0)

    @property
    def worldGeometry(self):
        return self.sketch.sketchToModelSpace(self.geometry)

    def move(self, v):
        raise NotSupported("moving sketch points")


def _pt(sketch, p):
    """A point argument for Fission: an existing sketch point's id, or [x, y] in mm."""
    if isinstance(p, SketchPoint):
        return p.id
    if isinstance(p, (list, tuple)):
        return [mm(p[0]), mm(p[1])]
    return [mm(p.x), mm(p.y)]


def _at(p):
    if isinstance(p, SketchPoint):
        return list(p._at)
    return [mm(p.x), mm(p.y)]


class SketchEntity(Base):
    def __init__(self, sketch, cid):
        self.sketch, self.id = sketch, cid
        self._construction = False

    @property
    def isConstruction(self):
        return self._construction

    @isConstruction.setter
    def isConstruction(self, on):
        if bool(on) != self._construction:
            _activate(self.sketch)
            call("sketch.construction", {"curves": [self.id]})
            self._construction = bool(on)

    @property
    def isFixed(self):
        return False

    @isFixed.setter
    def isFixed(self, v):
        pass

    def deleteMe(self):
        _activate(self.sketch)
        call("sketch.delete", {"curves": [self.id]})
        return True


class SketchLine(SketchEntity):
    def __init__(self, sketch, cid, a, b):
        super().__init__(sketch, cid)
        self.startSketchPoint, self.endSketchPoint = a, b

    @property
    def length(self):
        a, b = self.startSketchPoint._at, self.endSketchPoint._at
        return _cm(math.dist(a, b))

    @property
    def geometry(self):
        return Line3D(self.startSketchPoint.geometry, self.endSketchPoint.geometry)


class SketchCircle(SketchEntity):
    def __init__(self, sketch, cid, centre, radius_mm):
        super().__init__(sketch, cid)
        self.centerSketchPoint, self._r = centre, radius_mm

    @property
    def radius(self):
        return _cm(self._r)

    @property
    def geometry(self):
        return Circle3D(self.centerSketchPoint.geometry, Vector3D(0, 0, 1), self.radius)


class SketchArc(SketchEntity):
    def __init__(self, sketch, cid, centre, a, b):
        super().__init__(sketch, cid)
        self.centerSketchPoint, self.startSketchPoint, self.endSketchPoint = centre, a, b


class SketchFittedSpline(SketchEntity):
    def __init__(self, sketch, cid, pts):
        super().__init__(sketch, cid)
        self.fitPoints = Lst(pts)
        self.startSketchPoint = pts[0] if pts else None
        self.endSketchPoint = pts[-1] if pts else None


class SketchLineList(Lst):
    pass


def _created(r):
    return (r or {}).get("created") or {}


class SketchLines(Base):
    def __init__(self, sketch):
        self.sketch = sketch

    def addByTwoPoints(self, a, b):
        sk = self.sketch
        _activate(sk)
        r = call("sketch.line", {"points": [_pt(sk, a), _pt(sk, b)]})
        c = _created(r)
        pts = c.get("points") or []
        cid = (c.get("curves") or [None])[0]
        start = a if isinstance(a, SketchPoint) else SketchPoint(sk, pts[0] if pts else None, _at(a))
        end_id = pts[-1] if pts else None
        end = b if isinstance(b, SketchPoint) else SketchPoint(sk, end_id, _at(b))
        ln = SketchLine(sk, cid, start, end)
        sk._curves.append(ln)
        return ln

    def _rect(self, cmd, params):
        sk = self.sketch
        _activate(sk)
        r = call(cmd, params) or {}
        sides = r.get("sides") or {}
        corners = r.get("corners") or {}
        info = {c["id"]: c for c in sk._inspect().get("curves", []) if isinstance(c, dict)}
        out = []
        for key in ("bottom", "right", "top", "left"):
            cid = sides.get(key)
            c = info.get(cid, {})
            a = SketchPoint(sk, c.get("start"), c.get("start_at") or [0, 0])
            b = SketchPoint(sk, c.get("end"), c.get("end_at") or [0, 0])
            ln = SketchLine(sk, cid, a, b)
            sk._curves.append(ln)
            out.append(ln)
        del corners
        return SketchLineList(out)

    def addTwoPointRectangle(self, a, b):
        return self._rect("sketch.rectangle.two_point", {"p0": _pt(self.sketch, a), "p1": _pt(self.sketch, b)})

    def addCenterPointRectangle(self, centre, corner):
        return self._rect("sketch.rectangle.center", {"center": _pt(self.sketch, centre), "corner": _pt(self.sketch, corner)})

    def addThreePointRectangle(self, a, b, c):
        return self._rect("sketch.rectangle.three_point", {"p0": _pt(self.sketch, a), "p1": _pt(self.sketch, b), "p2": _pt(self.sketch, c)})

    def item(self, i):
        return [c for c in self.sketch._curves if isinstance(c, SketchLine)][i]

    @property
    def count(self):
        return len([c for c in self.sketch._curves if isinstance(c, SketchLine)])

    def __iter__(self):
        return iter([c for c in self.sketch._curves if isinstance(c, SketchLine)])


class SketchCircles(Base):
    def __init__(self, sketch):
        self.sketch = sketch

    def _from(self, r, centre_at, radius_mm):
        sk = self.sketch
        c = _created(r)
        cid = (c.get("curves") or [None])[0]
        info = next((x for x in sk._inspect().get("curves", []) if x.get("id") == cid), {})
        centre = SketchPoint(sk, info.get("center"), info.get("center_at") or centre_at)
        circ = SketchCircle(sk, cid, centre, info.get("radius", radius_mm))
        sk._curves.append(circ)
        return circ

    def addByCenterRadius(self, centre, radius):
        sk = self.sketch
        _activate(sk)
        r = call("sketch.circle.center", {"center": _pt(sk, centre), "radius": mm(radius)})
        return self._from(r, _at(centre), mm(radius))

    def addByTwoPoints(self, a, b):
        sk = self.sketch
        _activate(sk)
        r = call("sketch.circle.two_point", {"p0": _pt(sk, a), "p1": _pt(sk, b)})
        pa, pb = _at(a), _at(b)
        return self._from(r, [(pa[0] + pb[0]) / 2, (pa[1] + pb[1]) / 2], math.dist(pa, pb) / 2)

    def addByThreePoints(self, a, b, c):
        sk = self.sketch
        _activate(sk)
        r = call("sketch.circle.three_point", {"p0": _pt(sk, a), "p1": _pt(sk, b), "p2": _pt(sk, c)})
        return self._from(r, _at(a), 0)

    def __iter__(self):
        return iter([c for c in self.sketch._curves if isinstance(c, SketchCircle)])

    @property
    def count(self):
        return len(list(iter(self)))

    def item(self, i):
        return list(iter(self))[i]


class SketchArcs(Base):
    def __init__(self, sketch):
        self.sketch = sketch

    def _from(self, r):
        sk = self.sketch
        c = _created(r)
        cid = (c.get("curves") or [None])[0]
        info = next((x for x in sk._inspect().get("curves", []) if x.get("id") == cid), {})
        p = lambda k: SketchPoint(sk, info.get(k), info.get(k + "_at") or [0, 0])  # noqa: E731
        arc = SketchArc(sk, cid, p("center"), p("start"), p("end"))
        sk._curves.append(arc)
        return arc

    def addByThreePoints(self, start, mid, end):
        sk = self.sketch
        _activate(sk)
        return self._from(call("sketch.arc.three_point", {"start": _pt(sk, start), "end": _pt(sk, end), "through": _pt(sk, mid)}))

    def addByCenterStartSweep(self, centre, start, sweep):
        sk = self.sketch
        _activate(sk)
        return self._from(call("sketch.arc.center_point", {"center": _pt(sk, centre), "start": _pt(sk, start), "sweep": math.degrees(sweep)}))

    def __iter__(self):
        return iter([c for c in self.sketch._curves if isinstance(c, SketchArc)])


class SketchFittedSplines(Base):
    def __init__(self, sketch):
        self.sketch = sketch

    def add(self, points):
        sk = self.sketch
        _activate(sk)
        pts = items(points)
        r = call("sketch.spline.fit_point", {"points": [_pt(sk, p) for p in pts]})
        c = _created(r)
        cid = (c.get("curves") or [None])[0]
        info = next((x for x in sk._inspect().get("curves", []) if x.get("id") == cid), {})
        ids, ats = info.get("points") or [], info.get("points_at") or []
        sps = [p if isinstance(p, SketchPoint) else SketchPoint(sk, ids[i] if i < len(ids) else None, ats[i] if i < len(ats) else _at(p))
               for i, p in enumerate(pts)]
        sp = SketchFittedSpline(sk, cid, sps)
        sk._curves.append(sp)
        return sp


class SketchPoints(Base):
    def __init__(self, sketch):
        self.sketch = sketch

    def add(self, p):
        sk = self.sketch
        _activate(sk)
        r = call("sketch.point", {"point": _pt(sk, p)})
        pts = _created(r).get("points") or [None]
        return SketchPoint(sk, pts[0], _at(p))


class SketchCurves(Base):
    def __init__(self, sketch):
        self.sketchLines = SketchLines(sketch)
        self.sketchCircles = SketchCircles(sketch)
        self.sketchArcs = SketchArcs(sketch)
        self.sketchFittedSplines = SketchFittedSplines(sketch)
        self._sketch = sketch

    @property
    def count(self):
        return len(self._sketch._curves)

    def item(self, i):
        return self._sketch._curves[i]

    def __iter__(self):
        return iter(list(self._sketch._curves))


class SketchTextInput(Base):
    def __init__(self, text, height_cm, position=None):
        self.text, self.height = text, height_cm
        self.position = position
        self.fontName = None
        self.textStyle = 0
        self.angle = 0.0
        self._box = None
        self.isHorizontalFlip = False
        self.isVerticalFlip = False

    def setAsMultiLine(self, a, b, halign, valign, spacing=0):
        self._box = (a, b, halign, valign)
        return True

    def setAsFitOnPath(self, path, isAbovePath=True):
        raise NotSupported("text on a path")

    def setAsAlongPath(self, path, isAbovePath, halign, spacing):
        raise NotSupported("text along a path")


class SketchText(SketchEntity):
    def __init__(self, sketch, cid, curves, text):
        super().__init__(sketch, cid)
        self._curve_ids = curves
        self.text = text

    @property
    def boundingBox(self):
        info = {c["id"]: c for c in self.sketch._inspect().get("curves", []) if isinstance(c, dict)}
        xs, ys = [], []
        for cid in self._curve_ids:
            c = info.get(cid, {})
            for k in ("start_at", "end_at", "center_at"):
                if c.get(k):
                    xs.append(c[k][0])
                    ys.append(c[k][1])
        if not xs:
            return BoundingBox3D()
        return BoundingBox3D(Point3D(_cm(min(xs)), _cm(min(ys)), 0), Point3D(_cm(max(xs)), _cm(max(ys)), 0))


class SketchTexts(Base):
    def __init__(self, sketch):
        self.sketch = sketch

    def createInput(self, text, height, position):
        return SketchTextInput(text, height, position)

    def createInput2(self, text, height):
        return SketchTextInput(text, height)

    def add(self, inp):
        sk = self.sketch
        _activate(sk)
        h = mm(inp.height)
        params = {"text": inp.text, "height": h}
        if inp._box:
            a, b, halign, valign = inp._box
            pa, pb = _at(a), _at(b)
            x0, x1 = sorted((pa[0], pb[0]))
            y0, y1 = sorted((pa[1], pb[1]))
            lines = max(1, inp.text.count("\n") + 1)
            block = h * (1 + 0.6 * (lines - 1) * 1.0) if lines > 1 else h
            x = {0: x0, 1: (x0 + x1) / 2, 2: x1}.get(halign, x0)
            y = {0: y1 - h, 1: (y0 + y1) / 2 - block / 2 + (block - h), 2: y0 + block - h}.get(valign, y0)
            params["at"] = [x, y]
            params["align"] = {0: "left", 1: "center", 2: "right"}.get(halign, "left")
        elif inp.position is not None:
            params["at"] = _at(inp.position)
        else:
            params["at"] = [0, 0]
        if inp.fontName:
            params["font"] = inp.fontName
        if inp.textStyle & TextStyles.TextStyleBold:
            params["bold"] = True
        if inp.textStyle & TextStyles.TextStyleItalic:
            params["italic"] = True
        if inp.angle:
            params["angle"] = math.degrees(inp.angle)
        try:
            r = call("sketch.text", params)
        except FissionError as e:
            if "font" not in str(e):
                raise
            # a font (or its bold or italic face) this computer doesn't have: Fission's own
            message("The font '{}' isn't installed here, so Fission's own font was used.".format(params.pop("font", "")))
            params.pop("bold", None)
            params.pop("italic", None)
            r = call("sketch.text", params)
        c = _created(r)
        t = SketchText(sk, (c.get("curves") or [None])[0], c.get("curves") or [], inp.text)
        sk._texts.append(t)
        return t

    def item(self, i):
        return self.sketch._texts[i]

    @property
    def count(self):
        return len(self.sketch._texts)


class AreaProperties(Base):
    def __init__(self, area_cm2, centroid):
        self.area = area_cm2
        self.centroid = centroid
        self.perimeter = 0.0


class Profile(Base):
    def __init__(self, sketch, index, info):
        self.parentSketch = sketch
        self.index = index
        self._info = info

    def areaProperties(self, accuracy=0):
        c = self._info.get("centroid_sketch") or [0, 0]
        world = self.parentSketch.sketchToModelSpace(Point3D(_cm(c[0]), _cm(c[1]), 0))
        return AreaProperties(self._info.get("area_mm2", 0.0) / 100.0, world)

    @property
    def profileLoops(self):
        return Lst([_Loop(True)] + [_Loop(False) for _ in self._info.get("holes") or []])

    @property
    def boundingBox(self):
        return self.parentSketch.boundingBox

    def _curves(self):
        return list(self._info.get("curves") or [])


class _Loop(Base):
    def __init__(self, outer):
        self.isOuter = outer


class Profiles(Lst):
    pass


class Sketch(Base):
    def __init__(self, comp, sid, name, plane):
        self.parentComponent = comp
        self.id, self.name = sid, name
        self._plane = plane  # (origin mm, x, y)
        self._curves, self._texts = [], []
        self.sketchCurves = SketchCurves(self)
        self.sketchPoints = SketchPoints(self)
        self.sketchTexts = SketchTexts(self)
        self.isVisible = True
        self.isLightBulbOn = True
        self.areDimensionsShown = True
        self.isComputeDeferred = False

    def _inspect(self):
        return call("sketch.inspect", {"sketch": self.id}) or {}

    @property
    def profiles(self):
        info = self._inspect()
        return Profiles([Profile(self, p.get("index", i), p) for i, p in enumerate(info.get("profiles") or [])])

    @property
    def sketchDimensions(self):
        return _Ignored()

    @property
    def geometricConstraints(self):
        return _Ignored()

    @property
    def originPoint(self):
        return SketchPoint(self, None, [0, 0])

    @property
    def transform(self):
        o, x, y = self._plane
        n = _cross(x, y)
        m = Matrix3D()
        for i in range(3):
            m.m[i][0], m.m[i][1], m.m[i][2], m.m[i][3] = x[i], y[i], n[i], _cm(o[i])
        return m

    def modelToSketchSpace(self, p):
        o, x, y = self._plane
        n = _cross(x, y)
        d = [mm(p.x) - o[0], mm(p.y) - o[1], mm(p.z) - o[2]]
        dot = lambda a, b: sum(i * j for i, j in zip(a, b))  # noqa: E731
        return Point3D(_cm(dot(d, x)), _cm(dot(d, y)), _cm(dot(d, n)))

    def sketchToModelSpace(self, p):
        o, x, y = self._plane
        n = _cross(x, y)
        q = [o[i] + mm(p.x) * x[i] + mm(p.y) * y[i] + mm(p.z) * n[i] for i in range(3)]
        return Point3D(*(_cm(v) for v in q))

    @property
    def boundingBox(self):
        return BoundingBox3D()

    def project(self, entity):
        raise NotSupported("projecting into a sketch")

    def deleteMe(self):
        return True


class _Ignored(Base):
    """Dimensions and constraints: Fission keeps the geometry where the script put it."""

    def __getattr__(self, name):
        return lambda *a, **k: None


class Sketches(Lst):
    def __init__(self, comp):
        super().__init__()
        self.comp = comp

    def add(self, plane, occurrence=None):
        _leave_sketch()
        if isinstance(plane, BRepFace):
            r = call("sketch.create", {"plane": {"face": plane._point_mm(), "body": plane.body.name}}) or {}
            info = call("sketch.inspect", {"sketch": r.get("sketch")}) or {}
            pl = info.get("plane") or {}
            frame = (pl.get("origin", [0, 0, 0]), pl.get("x_dir", [1, 0, 0]), pl.get("y_dir", [0, 1, 0]))
        else:
            frame = (plane.origin, plane.x, plane.y)
            r = call("sketch.create", {"plane": {"origin": list(frame[0]), "x_dir": list(frame[1]), "y_dir": list(frame[2])}}) or {}
        sid = r.get("sketch") or r.get("id") or r.get("feature")
        sk = Sketch(self.comp, sid, r.get("name", "Sketch"), frame)
        _STATE["active"] = sk
        self._items.append(sk)
        return sk


# ---------------------------------------------------------------- bodies, faces, edges
class BRepBody(Base):
    def __init__(self, name):
        self._name = name
        self.isVisible = True
        self.isLightBulbOn = True
        self.appearance = None

    @property
    def name(self):
        return self._name

    @name.setter
    def name(self, new):
        if new and new != self._name:
            _leave_sketch()
            call("body.rename", {"body": self._name, "name": new})
            self._name = new

    @property
    def boundingBox(self):
        r = call("inspect.measure", {"bodies": [self._name]}) or {}
        per = r.get("bodies") or []
        b = next((x for x in per if x.get("name") == self._name), per[0] if per else {})
        bb = b.get("bbox") or [[0, 0, 0], [0, 0, 0]]
        lo, hi = (bb.get("min"), bb.get("max")) if isinstance(bb, dict) else (bb[0], bb[1])
        return BoundingBox3D(Point3D(*(_cm(v) for v in lo)), Point3D(*(_cm(v) for v in hi)))

    @property
    def faces(self):
        r = call("model.faces", {"body": self._name}) or {}
        return Lst([BRepFace(self, f) for f in r.get("faces") or []])

    @property
    def edges(self):
        r = call("model.edges", {"body": self._name}) or {}
        # Fusion shows a cylinder as one face: the seams Fission splits it along aren't edges there
        return Lst([BRepEdge(self, e) for e in r.get("edges") or [] if not e.get("seam")])

    @property
    def volume(self):
        r = call("inspect.measure", {"bodies": [self._name]}) or {}
        per = r.get("bodies") or []
        return (per[0].get("volume_mm3", 0.0) / 1000.0) if per else 0.0

    @property
    def isSolid(self):
        return True

    def deleteMe(self):
        _leave_sketch()
        call("solid.remove", {"bodies": [self._name]})
        return True

    def __eq__(self, o):
        return isinstance(o, BRepBody) and o._name == self._name

    def __hash__(self):
        return hash(self._name)


class BRepFace(Base):
    def __init__(self, body, info):
        self.body, self._info = body, info

    def _point_mm(self):
        return list(self._info.get("point") or self._info.get("centroid") or [0, 0, 0])

    def _normal(self):
        return list(self._info.get("plane_normal") or [0, 0, 1])

    @property
    def pointOnFace(self):
        return Point3D(*(_cm(v) for v in self._point_mm()))

    @property
    def centroid(self):
        return Point3D(*(_cm(v) for v in (self._info.get("centroid") or self._point_mm())))

    @property
    def area(self):
        return self._info.get("area", 0.0) / 100.0

    @property
    def geometry(self):
        cyl = self._info.get("cylinder")
        if cyl:
            return Cylinder(Point3D(*(_cm(v) for v in cyl.get("origin", [0, 0, 0]))), Vector3D(*cyl.get("axis", [0, 0, 1])), _cm(cyl.get("radius", 0)))
        return Plane(self.centroid, Vector3D(*self._normal()))

    @property
    def edges(self):
        return self.body.edges

    @property
    def boundingBox(self):
        p = self.pointOnFace
        return BoundingBox3D(p, p.copy())


class BRepEdge(Base):
    def __init__(self, body, info):
        self.body, self._info = body, info

    def _mid_mm(self):
        return list(self._info.get("mid") or [0, 0, 0])

    @property
    def pointOnEdge(self):
        return Point3D(*(_cm(v) for v in self._mid_mm()))

    @property
    def length(self):
        return _cm(self._info.get("length", 0.0))

    @property
    def geometry(self):
        c = self._info.get("circle")
        if c:
            return Circle3D(Point3D(*(_cm(v) for v in c["center"])), Vector3D(*c["normal"]), _cm(c["radius"]))
        a = self._info.get("start") or [0, 0, 0]
        b = self._info.get("end") or [0, 0, 0]
        return Line3D(Point3D(*(_cm(v) for v in a)), Point3D(*(_cm(v) for v in b)))

    @property
    def boundingBox(self):
        bb = self._info.get("bbox") or [self._mid_mm(), self._mid_mm()]
        return BoundingBox3D(Point3D(*(_cm(v) for v in bb[0])), Point3D(*(_cm(v) for v in bb[1])))

    @property
    def startVertex(self):
        return _Vertex(self._info.get("start"))

    @property
    def endVertex(self):
        return _Vertex(self._info.get("end"))


class _Vertex(Base):
    def __init__(self, p):
        self.geometry = Point3D(*(_cm(v) for v in (p or [0, 0, 0])))


class BRepBodies(Lst):
    pass


# ---------------------------------------------------------------- features
class Feature(Base):
    def __init__(self, reply):
        self._r = reply or {}
        self.name = self._r.get("name", "")

    @property
    def bodies(self):
        names = self._r.get("created_bodies") or self._r.get("changed_bodies") or []
        return Lst([BRepBody(n) for n in names])

    @property
    def faces(self):
        out = []
        for b in self.bodies:
            out.extend(b.faces)
        return Lst(out)

    @property
    def sideFaces(self):
        return self.faces

    @property
    def endFaces(self):
        return self.faces

    @property
    def startFaces(self):
        return self.faces

    def deleteMe(self):
        return True


def _profile_spec(profile):
    """What solid.extrude/revolve take for these profiles (all of one sketch)."""
    profs = items(profile)
    if not profs:
        raise NotSupported("a feature without a profile")
    first = profs[0]
    if isinstance(first, SketchText):
        sk = first.sketch
        mine = set()
        for t in profs:
            mine.update(t._curve_ids)
        info = sk._inspect().get("profiles") or []
        picked = [p.get("index", i) for i, p in enumerate(info) if set(p.get("curves") or []) <= mine]
        return sk, (picked if picked else "all")
    sk = first.parentSketch
    return sk, [p.index for p in profs]


class DistanceExtentDefinition(Base):
    def __init__(self, d):
        self.distance = d

    @staticmethod
    def create(d):
        return DistanceExtentDefinition(d)


class ThroughAllExtentDefinition(Base):
    @staticmethod
    def create():
        return ThroughAllExtentDefinition()


class ToEntityExtentDefinition(Base):
    def __init__(self, entity, chained=False, offset=None):
        self.entity, self.offset = entity, offset

    @staticmethod
    def create(entity, chained=False, offset=None):
        return ToEntityExtentDefinition(entity, chained, offset)


class AngleExtentDefinition(Base):
    def __init__(self, a):
        self.angle = a

    @staticmethod
    def create(a):
        return AngleExtentDefinition(a)


class ExtrudeFeatureInput(Base):
    def __init__(self, profile, operation):
        self.profile, self.operation = profile, operation
        self._p = {}
        self.participantBodies = []
        self.isSolid = True
        self.creationOccurrence = None
        self.startExtent = None
        self.taperAngle = None

    def setDistanceExtent(self, isSymmetric, distance):
        self._p = {"distance": length_of(distance)}
        if isSymmetric:
            self._p["direction"] = "symmetric"
        return True

    def setSymmetricExtent(self, distance, isFullLength, taper=None):
        d = length_of(distance)
        if not isFullLength:
            d = d * 2 if isinstance(d, (int, float)) else "2 * ({})".format(d)
        self._p = {"distance": d, "direction": "symmetric"}
        return True

    def setOneSideExtent(self, extent, direction, taper=None):
        if isinstance(extent, ThroughAllExtentDefinition):
            self._p = {"through_all": True}
        elif isinstance(extent, DistanceExtentDefinition):
            self._p = {"distance": length_of(extent.distance)}
        elif isinstance(extent, ToEntityExtentDefinition):
            e = extent.entity
            to = e._point_mm() if isinstance(e, BRepFace) else None
            if to is None:
                raise NotSupported("extruding to that kind of object")
            self._p = {"to": to}
        else:
            raise NotSupported("that extent")
        if direction == ExtentDirections.NegativeExtentDirection:
            self._p["direction"] = "negative"
        elif direction == ExtentDirections.SymmetricExtentDirection:
            self._p["direction"] = "symmetric"
        return True

    def setTwoSidesExtent(self, one, two, t1=None, t2=None):
        self._p = {"distance": length_of(one.distance), "distance2": length_of(two.distance)}
        return True

    def setAllExtent(self, direction):
        self._p = {"through_all": True}
        if direction == ExtentDirections.SymmetricExtentDirection:
            self._p["direction"] = "symmetric"
        elif direction == ExtentDirections.NegativeExtentDirection:
            self._p["direction"] = "negative"
        return True


def _bodies_of(r):
    return [BRepBody(n) for n in (r or {}).get("created_bodies") or []]


class ExtrudeFeatures(Lst):
    def createInput(self, profile, operation):
        return ExtrudeFeatureInput(profile, operation)

    def add(self, inp):
        sk, profs = _profile_spec(inp.profile)
        _leave_sketch()
        params = dict(inp._p)
        params.update({"sketch": sk.id, "profiles": profs, "operation": _OPS.get(inp.operation, "new")})
        if inp.taperAngle is not None:
            params["taper"] = angle_of(inp.taperAngle)
        if inp.participantBodies:
            params["targets"] = [b.name for b in items(inp.participantBodies)]
        f = Feature(call("solid.extrude", params))
        self._items.append(f)
        return f

    def addSimple(self, profile, distance, operation):
        inp = self.createInput(profile, operation)
        inp.setDistanceExtent(False, distance)
        return self.add(inp)


class RevolveFeatureInput(Base):
    def __init__(self, profile, axis, operation):
        self.profile, self.axis, self.operation = profile, axis, operation
        self._p = {}
        self.participantBodies = []

    def setAngleExtent(self, isSymmetric, angle):
        self._p = {"angle": angle_of(angle)}
        if isSymmetric:
            self._p["direction"] = "symmetric"
        return True

    def setTwoSideAngleExtent(self, a1, a2):
        self._p = {"angle": angle_of(a1), "angle2": angle_of(a2)}
        return True


def _axis_spec(axis, sketch):
    if isinstance(axis, SketchLine):
        return axis.id
    if isinstance(axis, ConstructionAxis):
        return axis.name
    if isinstance(axis, BRepEdge):
        return {"edge": axis._mid_mm()}
    raise NotSupported("that kind of axis")


class RevolveFeatures(Lst):
    def createInput(self, profile, axis, operation):
        return RevolveFeatureInput(profile, axis, operation)

    def add(self, inp):
        sk, profs = _profile_spec(inp.profile)
        axis = _axis_spec(inp.axis, sk)
        _leave_sketch()
        params = dict(inp._p) or {"angle": "360 deg"}
        params.update({"sketch": sk.id, "profiles": profs, "axis": axis, "operation": _OPS.get(inp.operation, "new")})
        if isinstance(inp.axis, SketchLine) and isinstance(axis, str):
            # Fission's revolve takes a sketch line's id with the sketch's own axes, or a world axis
            pass
        if inp.participantBodies:
            params["targets"] = [b.name for b in items(inp.participantBodies)]
        f = Feature(call("solid.revolve", params))
        self._items.append(f)
        return f


class CombineFeatureInput(Base):
    def __init__(self, target, tools):
        self.targetBody, self.toolBodies = target, tools
        self.operation = FeatureOperations.JoinFeatureOperation
        self.isKeepToolBodies = False
        self.isNewComponent = False


class CombineFeatures(Lst):
    def createInput(self, target, tools):
        return CombineFeatureInput(target, tools)

    def add(self, inp):
        _leave_sketch()
        r = call("solid.combine", {"target": inp.targetBody.name, "tools": [b.name for b in items(inp.toolBodies)],
                                   "operation": _OPS.get(inp.operation, "join"), "keep_tools": bool(inp.isKeepToolBodies)})
        f = Feature(r)
        self._items.append(f)
        return f


class MoveFeatureInput(Base):
    def __init__(self, entities, transform):
        self.inputEntities, self.transform = entities, transform

    def defineAsFreeMove(self, transform):
        self.transform = transform
        return True

    def defineAsTranslateXYZ(self, x, y, z, isDesignSpace=True):
        m = Matrix3D()
        m.translation = Vector3D(*(v.realValue if isinstance(v, ValueInput) else float(v) for v in (x, y, z)))
        self.transform = m
        return True


class MoveFeatures(Lst):
    def createInput(self, entities, transform):
        return MoveFeatureInput(entities, transform)

    def createInput2(self, entities):
        return MoveFeatureInput(entities, Matrix3D())

    def add(self, inp):
        _leave_sketch()
        bodies = [b.name for b in items(inp.inputEntities) if isinstance(b, BRepBody)]
        m = inp.transform
        axis, deg = m.axis_angle()
        params = {"bodies": bodies, "translate": [mm(m.m[0][3]), mm(m.m[1][3]), mm(m.m[2][3])]}
        if abs(deg) > 1e-9:
            params["axis"] = axis
            params["angle"] = "{} deg".format(deg)
        f = Feature(call("solid.move", params))
        self._items.append(f)
        return f


class ThreadInfo(Base):
    def __init__(self, internal, kind, designation, cls):
        self.isInternal, self.threadType, self.threadDesignation, self.threadClass = internal, kind, designation, cls


class ThreadDataQuery(Base):
    # ISO metric coarse pitches by nominal size (mm)
    _COARSE = {1: 0.25, 1.2: 0.25, 1.6: 0.35, 2: 0.4, 2.5: 0.45, 3: 0.5, 4: 0.7, 5: 0.8, 6: 1.0, 8: 1.25, 10: 1.5,
               12: 1.75, 14: 2.0, 16: 2.0, 18: 2.5, 20: 2.5, 22: 2.5, 24: 3.0, 27: 3.0, 30: 3.5, 33: 3.5, 36: 4.0}

    def recommendThreadData(self, diameter_cm, internal, kind="ISO Metric profile"):
        d = mm(diameter_cm)
        size = min(self._COARSE, key=lambda s: abs(s - d))
        pitch = self._COARSE[size]
        s = int(size) if float(size).is_integer() else size
        return (True, "M{}x{}".format(s, pitch if not float(pitch).is_integer() else int(pitch)), "6H" if internal else "6g")

    @property
    def allThreadTypes(self):
        return ["ISO Metric profile"]


class ThreadFeatureInput(Base):
    def __init__(self, faces, info):
        self.inputEntities, self.threadInfo = faces, info
        self.isFullLength = True
        self.isModeled = False
        self.threadLength = None
        self.threadOffset = None
        self.isRightHanded = True

    def setThreadLength(self, v):
        self.threadLength = v
        self.isFullLength = False
        return True


class ThreadFeatures(Lst):
    @property
    def threadDataQuery(self):
        return ThreadDataQuery()

    def createThreadInfo(self, internal, kind, designation, cls):
        return ThreadInfo(internal, kind, designation, cls)

    def createInput(self, faces, info):
        return ThreadFeatureInput(faces, info)

    def add(self, inp):
        _leave_sketch()
        last = None
        for face in items(inp.inputEntities):
            params = {"face": face._point_mm(), "designation": inp.threadInfo.threadDesignation, "modeled": bool(inp.isModeled),
                      "handedness": "right" if inp.isRightHanded else "left"}
            if not inp.isFullLength and inp.threadLength is not None:
                params["length"] = length_of(inp.threadLength)
            last = Feature(call("solid.thread", params))
            self._items.append(last)
        return last


class FilletFeatureInput(Base):
    def __init__(self):
        self._sets = []
        self.isRollingBallCorner = True

    def addConstantRadiusEdgeSet(self, edges, radius, isTangentChain=True):
        self._sets.append((items(edges), radius))
        return True

    @property
    def edgeSetInputs(self):
        return self


class FilletFeatures(Lst):
    def createInput(self):
        return FilletFeatureInput()

    def add(self, inp):
        _leave_sketch()
        last = None
        for edges, radius in inp._sets:
            by_body = {}
            for e in edges:
                by_body.setdefault(e.body.name, []).append(e._mid_mm())
            for body, pts in by_body.items():
                last = Feature(call("solid.fillet", {"edges": pts, "radius": length_of(radius), "body": body}))
                self._items.append(last)
        return last


class ChamferFeatureInput(Base):
    def __init__(self, edges, tangent=True):
        self.edges = items(edges)
        self._d = None
        self._d2 = None
        self._angle = None

    def setToEqualDistance(self, d):
        self._d = d
        return True

    def setToTwoDistances(self, d1, d2):
        self._d, self._d2 = d1, d2
        return True

    def setToDistanceAndAngle(self, d, a):
        self._d, self._angle = d, a
        return True


class ChamferFeatures(Lst):
    def createInput(self, edges, isTangentChain=True):
        return ChamferFeatureInput(edges, isTangentChain)

    def createInput2(self):
        return ChamferFeatureInput([])

    def add(self, inp):
        _leave_sketch()
        by_body = {}
        for e in inp.edges:
            by_body.setdefault(e.body.name, []).append(e._mid_mm())
        last = None
        for body, pts in by_body.items():
            params = {"edges": pts, "distance": length_of(inp._d), "body": body}
            if inp._d2 is not None:
                params["distance2"] = length_of(inp._d2)
            if inp._angle is not None:
                params["angle"] = angle_of(inp._angle)
            last = Feature(call("solid.chamfer", params))
            self._items.append(last)
        return last


class ShellFeatureInput(Base):
    def __init__(self, entities, tangent=True):
        self.inputEntities = items(entities)
        self.insideThickness = None
        self.outsideThickness = None


class ShellFeatures(Lst):
    def createInput(self, entities, isTangentChain=True):
        return ShellFeatureInput(entities, isTangentChain)

    def add(self, inp):
        _leave_sketch()
        faces = [f for f in inp.inputEntities if isinstance(f, BRepFace)]
        bodies = [b for b in inp.inputEntities if isinstance(b, BRepBody)]
        params = {}
        if inp.insideThickness is not None:
            params.update({"thickness": length_of(inp.insideThickness), "direction": "inside"})
        else:
            params.update({"thickness": length_of(inp.outsideThickness), "direction": "outside"})
        if faces:
            params["faces"] = [f._point_mm() for f in faces]
            params["body"] = faces[0].body.name
        else:
            params["faces"] = []
            params["body"] = bodies[0].name if bodies else None
        f = Feature(call("solid.shell", params))
        self._items.append(f)
        return f


class _PatternInput(Base):
    def __init__(self, entities, axis=None):
        self.inputEntities, self.axis = items(entities), axis
        self.quantity = ValueInput.createByReal(2)
        self.totalAngle = ValueInput.createByString("360 deg")
        self.isSymmetric = False
        self._dir1 = None

    def setDirectionTwo(self, *a):
        raise NotSupported("a second pattern direction")


def _pattern_targets(entities):
    bodies = [b.name for b in entities if isinstance(b, BRepBody)]
    if bodies:
        return {"bodies": bodies}
    feats = [f.name for f in entities if isinstance(f, Feature)]
    return {"features": feats}


class CircularPatternFeatures(Lst):
    def createInput(self, entities, axis):
        return _PatternInput(entities, axis)

    def add(self, inp):
        _leave_sketch()
        params = _pattern_targets(inp.inputEntities)
        ax = inp.axis
        params["axis"] = ax.name if isinstance(ax, ConstructionAxis) else _axis_spec(ax, None)
        params["count"] = int(round(float(inp.quantity.number())))
        params["angle"] = angle_of(inp.totalAngle)
        f = Feature(call("solid.pattern.circular", params))
        self._items.append(f)
        return f


class RectangularPatternFeatureInput(_PatternInput):
    def __init__(self, entities, dir1, q1, d1, kind):
        super().__init__(entities)
        self.directionOneEntity, self.quantityOne, self.distanceOne, self.patternDistanceType = dir1, q1, d1, kind


class RectangularPatternFeatures(Lst):
    def createInput(self, entities, dir1, quantity1, distance1, kind=1):
        return RectangularPatternFeatureInput(entities, dir1, quantity1, distance1, kind)

    def add(self, inp):
        _leave_sketch()
        params = _pattern_targets(inp.inputEntities)
        d = inp.directionOneEntity
        direction = d.direction if isinstance(d, ConstructionAxis) else [1, 0, 0]
        n = int(round(float(inp.quantityOne.number())))
        spacing = length_of(inp.distanceOne)
        if inp.patternDistanceType == PatternDistanceType.ExtentPatternDistanceType and isinstance(spacing, (int, float)) and n > 1:
            spacing = spacing / (n - 1)
        params.update({"dir1": direction, "count1": n, "spacing1": spacing})
        f = Feature(call("solid.pattern.rectangular", params))
        self._items.append(f)
        return f


class MirrorFeatureInput(Base):
    def __init__(self, entities, plane):
        self.inputEntities, self.mirrorPlane = items(entities), plane
        self.isCombine = False


class MirrorFeatures(Lst):
    def createInput(self, entities, plane):
        return MirrorFeatureInput(entities, plane)

    def add(self, inp):
        _leave_sketch()
        params = _pattern_targets(inp.inputEntities)
        pl = inp.mirrorPlane
        if isinstance(pl, ConstructionPlane):
            params["plane"] = {"origin": pl.origin, "x_dir": pl.x, "y_dir": pl.y}
        elif isinstance(pl, BRepFace):
            params["plane"] = {"origin": pl._point_mm(), "normal": pl._normal()}
        params["combine"] = bool(inp.isCombine)
        f = Feature(call("solid.mirror", params))
        self._items.append(f)
        return f


class OffsetFacesFeatureInput(Base):
    def __init__(self, faces, distance):
        self.entities, self.distance = items(faces), distance


class OffsetFacesFeatures(Lst):
    def createInput(self, faces, distance):
        return OffsetFacesFeatureInput(faces, distance)

    def add(self, inp):
        _leave_sketch()
        by_body = {}
        for f in inp.entities:
            by_body.setdefault(f.body.name, []).append(f._point_mm())
        last = None
        for body, pts in by_body.items():
            last = Feature(call("solid.offset_face", {"faces": pts, "distance": length_of(inp.distance), "body": body}))
            self._items.append(last)
        return last


class _Unsupported(Lst):
    def __init__(self, what):
        super().__init__()
        self._what = what

    def createInput(self, *a, **k):
        raise NotSupported(self._what)

    def add(self, *a, **k):
        raise NotSupported(self._what)


class Features(Base):
    def __init__(self):
        self.extrudeFeatures = ExtrudeFeatures()
        self.revolveFeatures = RevolveFeatures()
        self.combineFeatures = CombineFeatures()
        self.moveFeatures = MoveFeatures()
        self.threadFeatures = ThreadFeatures()
        self.filletFeatures = FilletFeatures()
        self.chamferFeatures = ChamferFeatures()
        self.shellFeatures = ShellFeatures()
        self.circularPatternFeatures = CircularPatternFeatures()
        self.rectangularPatternFeatures = RectangularPatternFeatures()
        self.mirrorFeatures = MirrorFeatures()
        self.offsetFacesFeatures = OffsetFacesFeatures()
        self.loftFeatures = _Unsupported("loft (not in Fission's Fusion scripts yet)")
        self.sweepFeatures = _Unsupported("sweep (not in Fission's Fusion scripts yet)")
        self.holeFeatures = _Unsupported("hole (not in Fission's Fusion scripts yet)")
        self.coilFeatures = _Unsupported("coil (not in Fission's Fusion scripts yet)")

    def __getattr__(self, name):
        # any other kind of feature (ribFeatures, draftFeatures, ...): a plain "not yet"
        if name.endswith("Features"):
            return _Unsupported("{} (not in Fission's Fusion scripts yet)".format(name[:-8]))
        raise AttributeError(name)


def _use(comp):
    """Make `comp` Fission's active component (where new sketches, features and bodies go)."""
    cid = getattr(comp, "_id", 0)
    if _STATE.get("component", 0) != cid:
        _leave_sketch()
        call("component.activate", {"component": cid if cid else "root"})
        _STATE["component"] = cid


class Component(Base):
    def __init__(self, name="Root", cid=0):
        self._name = name
        self._id = cid
        self._sketches = Sketches(self)
        self._features = Features()
        self._planes = ConstructionPlanes()
        self.xYConstructionPlane = _std_plane("XY")
        self.xZConstructionPlane = _std_plane("XZ")
        self.yZConstructionPlane = _std_plane("YZ")
        self.xConstructionAxis = ConstructionAxis("X", [1, 0, 0])
        self.yConstructionAxis = ConstructionAxis("Y", [0, 1, 0])
        self.zConstructionAxis = ConstructionAxis("Z", [0, 0, 1])
        self.originConstructionPoint = ConstructionPoint()
        self.occurrences = Occurrences(self)
        self.description = ""
        self.partNumber = ""

    @property
    def name(self):
        return self._name

    @name.setter
    def name(self, new):
        new = str(new)
        if self._id and new and new != self._name:
            _leave_sketch()
            call("component.rename", {"component": self._id, "name": new})
        self._name = new

    @property
    def sketches(self):
        _use(self)
        return self._sketches

    @property
    def features(self):
        _use(self)
        return self._features

    @property
    def constructionPlanes(self):
        _use(self)
        return self._planes

    @property
    def extrudeFeatures(self):
        return self.features.extrudeFeatures

    @property
    def revolveFeatures(self):
        return self.features.revolveFeatures

    @property
    def bRepBodies(self):
        _leave_sketch()
        r = call("component.list", {}) or {}
        mine = next((c for c in r.get("components") or [] if c.get("id") == self._id), {})
        return BRepBodies([BRepBody(n) for n in mine.get("bodies") or []])

    @property
    def allOccurrences(self):
        out = []
        for o in self.occurrences:
            out.append(o)
            out.extend(o.component.allOccurrences)
        return Lst(out)

    def __eq__(self, o):
        return isinstance(o, Component) and o._id == self._id

    def __hash__(self):
        return hash(self._id)


class Occurrence(Base):
    def __init__(self, comp, oid, name):
        self.component, self._id, self.name = comp, oid, name
        self.isLightBulbOn = True
        self.isVisible = True

    @property
    def fullPathName(self):
        return self.name

    @property
    def bRepBodies(self):
        return self.component.bRepBodies

    @property
    def isGrounded(self):
        return bool(_STATE.get("grounded", {}).get(self._id, False))

    @isGrounded.setter
    def isGrounded(self, on):
        _leave_sketch()
        call("occurrence.ground", {"occurrence": self._id, "grounded": bool(on)})
        _STATE.setdefault("grounded", {})[self._id] = bool(on)

    def _move(self, m):
        rows = [[m.m[i][0], m.m[i][1], m.m[i][2], mm(m.m[i][3])] for i in range(3)]
        call("occurrence.move", {"occurrence": self._id, "matrix": rows, "space": "world", "move_grounded": True})

    @property
    def transform(self):
        return _STATE.get("placed", {}).get(self._id) or Matrix3D()

    @transform.setter
    def transform(self, m):
        _leave_sketch()
        old = self.transform.copy()
        old.invert()
        # move by m * old^-1: from where it is to m
        delta = Matrix3D()
        delta.m = [[sum(m.m[i][k] * old.m[k][j] for k in range(4)) for j in range(4)] for i in range(4)]
        identity = all(abs(delta.m[i][j] - (1.0 if i == j else 0.0)) < 1e-12 for i in range(3) for j in range(4))
        if not identity:
            self._move(delta)
        _STATE.setdefault("placed", {})[self._id] = m.copy()


class Occurrences(Lst):
    def __init__(self, parent):
        super().__init__()
        self.parent = parent

    def addNewComponent(self, transform=None):
        _use(self.parent)
        n = len(_STATE.setdefault("components", [])) + 1
        r = call("component.create", {"name": "Component{}".format(n), "activate": False}) or {}
        comp = Component("Component{}".format(n), r.get("component", 0))
        occ = Occurrence(comp, r.get("occurrence"), "Component{}:1".format(n))
        _STATE["components"].append(comp)
        if transform is not None:
            occ.transform = transform
        self._items.append(occ)
        return occ

