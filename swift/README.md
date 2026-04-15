# Swift Surface Layout

The Apple code in this repo is split intentionally.

- `fileprovider/`
  - macOS-oriented packaging lane
  - FileProvider bundle assembly
  - Finder-related integration and notarization helpers
- `ios/`
  - iOS host app and FileProvider extension
  - xcodegen project spec
  - manual build and TestFlight upload tooling

Both surfaces are experimental as of M9.

The current contract is:

- keep them buildable
- keep packaging scripts usable for manual validation
- avoid presenting them as continuously proven release surfaces until stronger
  simulator, Finder, and distribution evidence exists
