# tcfs Architecture

The full architecture document is maintained as a LaTeX source file and
distributed as PDF.

- **Source**: [`docs/tex/architecture.tex`](tex/architecture.tex)
- **PDF**: Built by CI and available as a [release artifact](https://github.com/tinyland-inc/tummycrypt/actions/workflows/docs.yml)

To build locally:

```bash
task docs:pdf
# Output: dist/docs/architecture.pdf
```

## Quick Reference

See the [Architecture PDF](https://github.com/tinyland-inc/tummycrypt/actions/workflows/docs.yml) for full details including:

- System architecture (client + server components)
- Crate map (13 workspace crates)
- Stub file format specification
- Hydration sequence
- Credential chain
- Phase roadmap
- Infrastructure layout
