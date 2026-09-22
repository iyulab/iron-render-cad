# Golden cases

`g1.expected.json` is a copy of what the `uncad-model` repository's golden writer produces
for the G1 spec: the model a reader must produce from the synthetic drawing. This crate
renders it -- no parser is involved -- so the determinism and reporting checks in `tests/`
run against a real drawing's worth of entities.

The file is generated, not hand-written. To regenerate after a change to the writer or the
spec, from a checkout of `uncad-model`:

```
cargo run -p uncad-model-golden --example write_case -- g1 g1.dxf g1.expected.json
```

and copy the JSON here. A tree that carries both repositories side by side checks that the
copy has not drifted from the writer.
