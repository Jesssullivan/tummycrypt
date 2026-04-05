# odrive Reverse Engineering - Logic & Architecture Documentation

Cleanroom analysis of odrive desktop sync client to document logic flows,
architecture patterns, and protocol specifications for tummycrypt feature parity.

**This directory contains ONLY documentation, scripts, and analysis output.**
**No odrive binaries or proprietary code are stored here.**

Artifacts referenced from `../odrive-cleanroom-re/` (gitignored, never committed).

## Directory Layout

```
odrive-re/
  docs/           # Architecture docs, protocol specs, logic flow diagrams
  scripts/        # Python/Ghidra scripts for automated analysis
  ghidra/         # Ghidra project files and exports (gitignored heavy files)
  symbols/        # Extracted symbol tables, string dumps, module maps
  analysis/
    linux-agent/  # Linux SyncAgent analysis (debug symbols, unstripped)
    mac-agent/    # macOS SyncAgent analysis
    windows-client/ # Windows PE analysis
    mac-pkg/      # macOS installer pkg analysis
    ipc-protocol/ # JSON-over-TCP protocol documentation
```

## Artifacts Inventory

| Artifact | Platform | Type | Notes |
|---|---|---|---|
| `odrive.py` | Cross-platform | Python CLI | JSON-over-TCP IPC, 1632 lines |
| `odriveSyncAgent-lnx-1045` | Linux x86_64 | ELF (PyInstaller) | **Unstripped, debug symbols** |
| `odriveSyncAgentClient-lnx-1045` | Linux x86_64 | ELF (PyInstaller) | Unstripped |
| `odriveSyncAgent-mac-977` | macOS x86_64 | Mach-O (PyInstaller) | Carbon+AppServices |
| `odriveSyncAgentClient-mac-977` | macOS x86_64 | Mach-O (PyInstaller) | |
| `odrivesync.7513.exe` | Windows x86 | PE (GUI) | 68MB |
| `odrivesync.7666.pkg` | macOS | xar installer | 50MB |

## Key Discovery

The Linux agent binary is **not stripped** and contains full Python 2.7 module paths.
This provides a complete architecture map of the odrive sync engine.
