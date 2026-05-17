# macOS FileProvider neo Strict Installed Preflight

Created: 2026-05-17T01:11:18Z

This packet is the first green strict preflight against the canonical
package-installed app on `neo`.

Verified paths:

- `tcfs`: `/usr/local/bin/tcfs` reporting `0.12.12`
- `tcfsd`: `/usr/local/bin/tcfsd` reporting `0.12.12`
- host app: `/Applications/TCFSProvider.app`
- FileProvider extension:
  `/Applications/TCFSProvider.app/Contents/Extensions/TCFSFileProvider.appex`

Strict preflight status: `0`

The host app and extension have valid codesign, App Group entitlements,
keychain access-group entitlements, embedded provisioning profiles, and one
visible PlugInKit registration parented by `/Applications/TCFSProvider.app`.

Claim boundary: this proves installed signing/profile/registration preflight.
It does not prove daemon storage health, FileProvider enumeration, or
hydration; those are covered by later packets.
