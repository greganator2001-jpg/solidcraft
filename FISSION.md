# Fission

Fission is Gregory's personal build of [SolidCraft](https://github.com/tangency-labs/solidcraft): the same
parametric CAD (same kernel, same results, same speed), plus the things his old FissionCAD app did for his
Bambu Lab printers.

- Branch: `fission` (upstream SolidCraft stays on `main`; updates from upstream are merged into `fission`).
- Builds: every push to `fission` builds `Fission-<version>-windows-x64.zip` and publishes it on this fork's
  Releases page (`.github/workflows/fission-windows.yml`). Version: `FISSION_VERSION`.
- Licence: SolidCraft's, MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE, NOTICE). Fission's additions are
  under the same terms.

## What Fission adds

- **Bambu printing** (desktop only): slicing with the installed Bambu Studio and its presets, including your
  own filaments; your printers read from Bambu Studio's settings; AMS sync by filament ID; Print plate over
  the local network, or Send with Bambu Studio. Commands: `bambu.*`.

Planned: Note for Claude, the sliced preview, running Fusion 360 scripts.
