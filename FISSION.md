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

- **The BAMBU tab** (desktop only), laid out like FissionCAD's: pick which bodies go on the plate, move, turn,
  scale, lay on a face, measure and auto-orient them (right-click a part for more), slice with the installed
  Bambu Studio and your presets (filaments editable per slot), preview the layers as toolpaths or solid
  beads, and print over the local network (printers added by IP and access code, found on the network, or
  read from Bambu Studio), or Send with Bambu Studio. Commands: `bambu.*`.
- **Fusion 360 scripts** (MANAGE > ADD-INS > Run Script): a Fusion Python script (`def run(context)`) runs
  in Python with Fission's stand-in for Fusion's API (`fusion/adsk`, next to Fission.exe). Needs Python 3.
  Command: `fusion.run_script`.
- **From Fusion** (MANAGE > ADD-INS > From Fusion): `Install Fusion add-in.cmd` puts the *Fission Export*
  script into Fusion; it saves the open design with its timeline as a `.ffx`, which Fission rebuilds step by
  step (sketches, features, parameters), or brings in as Fusion's finished bodies where it can't. Command:
  `fusion.open_ffx`.
- **Note for Claude** (Help menu): notes kept in `~/Fission/notes-for-claude.md`, each with a folder of
  evidence if wanted (a screenshot, a copy of the design, what Fission was doing); Fix it now copies the
  request and opens Claude.
- **Sketch guides**: while drawing or dragging, a point lines up with other points, the middles of lines and
  the origin on dotted guides, so a centre stays centred.
- **The Fission colourway** (Settings > Theme), and the FissionCraft name in the title bar.
