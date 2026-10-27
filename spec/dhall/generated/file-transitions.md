# File machine transitions

Generated from spec/dhall/File.dhall by spec/check-dhall.sh --write. Do not
edit by hand. Each cell is the state after the event.

| state | Enumerated | Open | FetchOk | Unreachable | LocalWrite | Flush | CasWon | CasLost | RemoteUpdated | RemoteTombstone | RemoteMissing | Unsync | Resolve |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Absent | Stub | Absent | Absent | Absent | Dirty | Absent | Absent | Absent | Stub | Absent | Absent | Absent | Absent |
| Stub | Stub | Hydrating | Stub | Stub | Hydrating | Stub | Stub | Stub | Stub | Absent | Stub | Stub | Stub |
| Hydrating | Hydrating | Hydrating | Clean | Stub | Hydrating | Hydrating | Hydrating | Hydrating | Hydrating | Absent | Hydrating | Stub | Hydrating |
| Clean | Clean | Clean | Clean | Clean | Dirty | Clean | Clean | Clean | Stub | Absent | Clean | Stub | Clean |
| Dirty | Dirty | Dirty | Dirty | Dirty | Dirty | Publishing | Dirty | Dirty | Dirty | Dirty | Dirty | Dirty | Dirty |
| Publishing | Publishing | Publishing | Publishing | Dirty | Dirty | Publishing | Clean | Conflict | Publishing | Publishing | Publishing | Publishing | Publishing |
| Conflict | Conflict | Conflict | Conflict | Conflict | Conflict | Conflict | Conflict | Conflict | Conflict | Conflict | Conflict | Conflict | Dirty |
