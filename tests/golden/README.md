# Golden cases

Each `<case>.expected.json` here is a copy of what the `uncad-model` repository's golden
writer produces for that case's spec: the model a reader must produce from the synthetic
drawing. This crate renders it -- no parser is involved -- so the determinism and reporting
checks in `tests/` run against a real drawing's worth of entities.

| Case | The drawing | Rendered by |
|---|---|---|
| `g1` | A plate: outline, holes, dimensions through their blocks, a title block with attributes | `determinism.rs` |
| `g2` | One line inside block references nested three deep, rotated and scaled at each level | `nested_blocks.rs` |
| `g11` | A mirrored part: a circle, an arc, a bulged polyline, a text, a SOLID and a block reference, all with normal (0, 0, -1) | `ocs.rs` |
| `g12` | Polylines that are not just their vertices: a slot, a tapered arrow, a rounded corner, a DONUT | `polylines.rs` |
| `g13` | Text in every justification, stretched and slanted, and a block's attributes, one invisible | `justified_text.rs`, `hidden.rs` |
| `g14` | A model on layers in every state, and a sheet of viewports onto it | `hidden.rs` |

The files are generated, not hand-written. To regenerate after a change to the writer or a
spec, from a checkout of `uncad-model`:

```
cargo run -p uncad-model-golden --example write_case -- <case> <case>.dxf <case>.expected.json
```

and copy the JSON here. A tree that carries both repositories side by side checks that the
copies have not drifted from the writer.
