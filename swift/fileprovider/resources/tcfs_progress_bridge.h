/* Supplementary header for progress callback (cbindgen doesn't export complex function pointer types) */

#ifndef TCFS_PROGRESS_BRIDGE_H
#define TCFS_PROGRESS_BRIDGE_H

#include <stdint.h>

typedef void (*TcfsProgressCallback)(uint64_t completed, uint64_t total, const void *context);

enum TcfsError tcfs_provider_fetch_with_progress(
    struct TcfsProvider *provider,
    const char *item_id,
    const char *dest_path,
    TcfsProgressCallback callback,
    const void *callback_context
);

#endif /* TCFS_PROGRESS_BRIDGE_H */
