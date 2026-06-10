/* raptor.h - Raptor Player C FFI Header
 * Generated reference. Do not edit manually.
 *
 * Usage:
 *   - Dart/Flutter: use dart:ffi to look up these symbols
 *   - C/C++: link against libraptor_ffi and include this header
 */

#ifndef RAPTOR_H
#define RAPTOR_H

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Opaque handle - Dart side only sees a pointer */
typedef struct RaptorHandle RaptorHandle;

/* Event callback: (event_json, user_data) */
typedef void (*RaptorEventCallback)(const char *event_json, void *user_data);

/* Property change callback: (value_json, user_data) */
typedef void (*RaptorPropertyCallback)(const char *value_json, void *user_data);

/* Create a new player instance. Returns an opaque handle. */
RaptorHandle *raptor_create(void);

/* Destroy a player instance and free all resources. */
void raptor_destroy(RaptorHandle *handle);

/* Send a command in JSON format.
 * Returns 0 on success, negative error code on failure.
 * See raptor_last_error() for details. */
int raptor_command(RaptorHandle *handle, const char *cmd_json);

/* Read a property by name. Returns a JSON string.
 * Caller must free the returned string with raptor_free_string().
 * Returns NULL if the property does not exist. */
char *raptor_get_property(RaptorHandle *handle, const char *name);

/* Set a property by name. value_json is a JSON-encoded value.
 * Returns 0 on success, negative error code on failure. */
int raptor_set_property(RaptorHandle *handle,
                        const char *name,
                        const char *value_json);

/* Subscribe to property changes.
 *
 * When the named property changes, the callback is invoked with the new value.
 * Returns observer ID (>= 0) on success, -1 on failure.
 *
 * Callback parameters: (value_json, user_data)
 * - value_json: JSON-encoded new value (valid only during callback, do not free)
 * - user_data: opaque pointer passed at registration time */
int64_t raptor_observe_property(RaptorHandle *handle,
                                const char *name,
                                RaptorPropertyCallback callback,
                                void *user_data);

/* Unsubscribe from property changes.
 *
 * Pass the observer ID returned by raptor_observe_property().
 * Returns 0 on success, negative error code on failure. */
int raptor_unobserve_property(RaptorHandle *handle, int64_t observer_id);

/* Register an event callback.
 *
 * Spawns a background thread that reads events and invokes the callback.
 * After calling this, raptor_poll_event() will always return NULL.
 *
 * Parameters:
 * - callback: function pointer (NULL is a no-op, does not cancel)
 * - user_data: opaque pointer passed to every callback invocation */
void raptor_set_event_callback(RaptorHandle *handle,
                               RaptorEventCallback callback,
                               void *user_data);

/* Poll for the next event (non-blocking).
 *
 * Returns a JSON string or NULL if no event is pending.
 * If raptor_set_event_callback() was called, this always returns NULL.
 * Caller must free the returned string with raptor_free_string(). */
char *raptor_poll_event(RaptorHandle *handle);

/* Get the GPU texture ID for the current video frame.
 * Intended for Flutter TextureWidget integration.
 * Returns -1 if no video is active. */
int64_t raptor_get_texture_id(RaptorHandle *handle);

/* Set a platform-native renderer handle.
 * Implementation-specific; reserved for future use. */
void raptor_set_renderer(RaptorHandle *handle, void *renderer);

/* Free a string allocated by raptor.
 * Pass the pointer returned by raptor_get_property() / raptor_poll_event() / etc. */
void raptor_free_string(char *s);

/* Get the last error message for the given handle.
 * Returns a JSON string with error details, or NULL if no error.
 * Caller must free with raptor_free_string(). */
char *raptor_last_error(RaptorHandle *handle);

#ifdef __cplusplus
}
#endif

#endif /* RAPTOR_H */
