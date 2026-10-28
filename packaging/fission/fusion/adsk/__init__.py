"""Fission's stand-in for Fusion 360's `adsk` package: `import adsk.core, adsk.fusion` in a Fusion script
gets these, which become Fission commands."""
from . import core, fusion  # noqa: F401
from .core import autoTerminate, doEvents, terminate  # noqa: F401
