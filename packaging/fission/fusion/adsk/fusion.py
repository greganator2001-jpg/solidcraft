"""adsk.fusion for Fusion scripts running in Fission."""
from ._impl import (AngleExtentDefinition, BRepBodies, BRepBody, BRepEdge, BRepFace, ChamferFeatureInput,  # noqa: F401
                    ChamferFeatures, ChamferTypes, CombineFeatureInput, CombineFeatures, Component, ConstructionAxis,
                    ConstructionPlane, ConstructionPlaneInput, ConstructionPoint, Design, DesignTypes,
                    DistanceExtentDefinition, ExtentDirections, ExtrudeFeatureInput, ExtrudeFeatures, Feature,
                    FeatureOperations, Features, FilletFeatureInput, FilletFeatures, MirrorFeatureInput, MoveFeatureInput,
                    NotSupported, Occurrence, PatternDistanceType, Profile, RevolveFeatureInput, RevolveFeatures, Sketch,
                    SketchArc, SketchCircle, SketchFittedSpline, SketchLine, SketchPoint, Sketches, SketchText,
                    SketchTextInput, SketchTexts, TextStyles, ThreadFeatureInput, ThreadFeatures,
                    ThroughAllExtentDefinition, ToEntityExtentDefinition, UserParameter, UserParameters)

ExtrudeFeature = RevolveFeature = FilletFeature = Feature
