# iron-render-cad

Deterministic rendering for CAD drawings: turns a drawing model into SVG or PNG, and draws a change set on top of the original as an overlay (a redline).

Built as a tool to be handed to an agent. It contains no AI of its own, and it is just as useful without an agent.

## What it does

- **Render** — drawing model → SVG / PNG.
- **Overlay** — (drawing model, change set) → the original with proposed or actual changes marked on top. The original geometry is drawn unchanged; the changes are a separate layer.
- **Give an agent something to look at** — when a semantic description is not enough, a rendered view is the fallback.

Input is expressed in the [uncad-model](https://github.com/iyulab/uncad-model) entity model; change sets come from [iron-diff-cad](https://github.com/iyulab/iron-diff-cad).

## What it is not

- Not a verifier. A render is for humans and for fallback viewing; it is never evidence that an edit is correct. Use the numeric diff for that.
- Not a file parser, and not a CAD file writer. It produces pictures, not drawings.
- Not an interactive viewer or UI component.
- Not an ML library. It performs no inference.

## Status

0.x. `to_svg` and `to_png` render a `uncad_model::CadDatabase` (model space, paper space or everything) -- `layout_to_svg` and `layout_to_png` one paper layout as its sheet, with the model shown through its viewports -- report the entity types they could not draw, the block references that drew nothing, what the bounds on the file's numbers left out and how much the drawing itself hides (layers off, frozen or not plotted, invisible entities), and are deterministic byte for byte. A malformed drawing may cost a missing entity and a note saying so, never the process. The overlay (change set on top of the original) is not implemented yet. The design principles are
in [docs/principles.md](docs/principles.md); what is approximated and what is unverified is in
[docs/CAVEATS.md](docs/CAVEATS.md).

```rust
let db: uncad_model::CadDatabase = /* from a parser, or from its JSON */;
let svg = iron_render_cad::to_svg(&db, iron_render_cad::ToSvgOptions::default());
std::fs::write("drawing.svg", svg.svg)?;
println!("not drawn: {:?}", svg.unsupported_types);
let png = iron_render_cad::to_png(&db, iron_render_cad::ToPngOptions::default())?;
std::fs::write("drawing.png", png.png)?;
```

## License

MIT
