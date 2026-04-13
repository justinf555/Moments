# XDG Photo Picker Portal — Interface Proposal

**Proposed interface name:** `org.freedesktop.portal.PhotoPicker`  
**Status:** Draft proposal  
**Origin implementation:** com.moments.Photos.Portal  
**Author:** Moments Photo App Project  
**Revision:** 1  

---

## Abstract

This document proposes a new XDG Desktop Portal interface, `org.freedesktop.portal.PhotoPicker`, that allows sandboxed and unsandboxed applications to request a user-selected set of photos from the host system's photo library manager, without requiring direct filesystem access or knowledge of the library's internal structure.

The interface follows the established XDG portal request/response pattern and is toolkit-neutral, desktop-environment-neutral, and vendor-neutral.

---

## Motivation

Applications that work with photos — commercial print services, photobook editors, social sharing tools, image editors — frequently need to let users select photos from their existing library. Today, each application solves this independently:

- Some embed their own import UI, duplicating effort and providing an inconsistent experience
- Some use `org.freedesktop.portal.FileChooser`, which exposes raw filesystem paths rather than library-aware metadata (albums, dates, tags, faces)
- Some require the user to manually export photos from their library first

A dedicated photo picker portal solves all three problems. The host photo application presents its native picker UI — which the user already knows — and returns selected photos to the calling application as exported file paths, with optional metadata. The calling application needs no knowledge of where or how photos are stored.

### Scope

This portal is a **source interface only**. Its responsibility ends the moment exported file paths are delivered to the calling application via the `Response` signal. What the calling application does with those files is entirely out of scope:

- A commercial photo print service will upload the files to its own vendor API over HTTPS
- A photobook editor will import them into its own layout engine
- A social sharing tool will upload them to a web service
- An image editor will open them locally

None of these downstream workflows involve D-Bus, XDG portals, or any further interaction with the photo library. The portal makes no assumptions about how files will be used after delivery, and deliberately provides no mechanism to influence or observe downstream processing. In particular, `org.freedesktop.portal.Print` — which sends documents to local or network printers — is unrelated to this interface and is not part of the photo print workflow this portal enables.

---

## Design Principles

1. **The host application owns the picker UI.** The calling application invokes the portal and waits; it does not render a picker of its own.
2. **File paths are the contract.** The portal returns exported copies at temporary paths. The calling application reads files, not library internals.
3. **Toolkit and desktop neutral.** No GTK, Qt, or GNOME/KDE-specific types appear in the interface.
4. **Async by design.** The request returns immediately; the response arrives via signal when the user completes selection.
5. **Backend abstraction.** The portal frontend (common interface) is separate from the backend (per-desktop implementation). Any photo library manager can implement the backend.
6. **Least privilege.** The calling application receives only what the user explicitly selects. It cannot enumerate the library, access unselected photos, or observe library changes.

---

## Versioning

The interface carries a `version` property. Callers should read this property on first connection and negotiate capability accordingly. This document describes version `1`.

Future versions may add new keys to option and response dictionaries. Implementations must ignore unknown keys to preserve forward compatibility.

---

## Interface Specification

### Service name

The portal frontend is registered at the well-known name:

```
org.freedesktop.portal.Desktop
```

Object path:

```
/org/freedesktop/portal/desktop
```

### Method: `PickPhotos`

Opens the host photo library's native picker UI and returns the user's selection.

```
PickPhotos (
  IN  parent_window  s,
  IN  options        a{sv},
  OUT handle         o
)
```

**Parameters:**

| Parameter | Type | Description |
|---|---|---|
| `parent_window` | `s` | Window identifier for the calling application. Used to parent the picker dialog. Format: `x11:XID` or `wayland:handle`. Pass empty string if unknown. |
| `options` | `a{sv}` | Dictionary of options (see below). |
| `handle` | `o` (out) | Object path of the request. Subscribe to `Response` signal on this object. |

**Options dictionary (`a{sv}`):**

| Key | Type | Default | Description |
|---|---|---|---|
| `handle_token` | `s` | auto | Caller-supplied token appended to the request handle path. Must be a valid D-Bus object path component. |
| `accept_label` | `s` | `"Select"` | Label for the picker's confirm button. |
| `title` | `s` | `"Select Photos"` | Title shown in the picker window. |
| `max_count` | `u` | `0` (unlimited) | Maximum number of photos the user may select. `0` means no limit. |
| `album_hint` | `s` | `""` | Album name or ID to pre-navigate the picker to on open. |
| `export_format` | `s` | `"jpeg"` | Format for exported files. One of: `"jpeg"`, `"png"`, `"tiff"`, `"original"`. |
| `export_quality` | `u` | `90` | JPEG quality, 1–100. Ignored for non-JPEG formats. |
| `max_dimension` | `u` | `0` (original) | Maximum pixel dimension (longest edge) for exported files. `0` means full resolution. |
| `strip_metadata` | `b` | `false` | Strip EXIF/XMP metadata from exported files if `true`. |
| `colour_profile` | `s` | `"sRGB"` | Colour profile for exported files. One of: `"sRGB"`, `"AdobeRGB"`, `"original"`. |
| `modal` | `b` | `true` | Whether the picker window should be modal to the parent. |

### Signal: `Response`

Emitted on the request handle object when the user confirms or cancels the picker.

```
Response (
  OUT response  u,
  OUT results   a{sv}
)
```

**Response codes:**

| Value | Meaning |
|---|---|
| `0` | Success — user confirmed selection |
| `1` | Cancelled — user dismissed the picker |
| `2` | Other error — see `results["error"]` |

**Results dictionary (`a{sv}`):**

| Key | Type | Condition | Description |
|---|---|---|---|
| `uris` | `as` | response = 0 | Array of `file://` URIs for the exported photo files, in selection order. |
| `count` | `u` | response = 0 | Number of photos selected. |
| `metadata` | `aa{sv}` | response = 0, if supported | Array of metadata dicts, one per photo, in the same order as `uris`. See metadata keys below. |
| `error` | `s` | response = 2 | Human-readable error description. |

**Per-photo metadata keys (`a{sv}`):**

Backends may return any subset of these. Callers must handle absent keys gracefully.

| Key | Type | Description |
|---|---|---|
| `title` | `s` | Photo title or filename |
| `date_taken` | `s` | ISO 8601 timestamp, e.g. `"2024-06-15T14:32:00"` |
| `width` | `u` | Original pixel width |
| `height` | `u` | Original pixel height |
| `camera_make` | `s` | EXIF camera manufacturer |
| `camera_model` | `s` | EXIF camera model |
| `latitude` | `d` | GPS latitude (degrees, WGS84) |
| `longitude` | `d` | GPS longitude (degrees, WGS84) |
| `rating` | `u` | User rating, 0–5 |
| `tags` | `as` | Array of user-assigned tag strings |

### Property: `version`

```
version  u  (read-only)
```

Version of the implemented interface. This document defines version `1`.

---

## D-Bus Introspection XML

```xml
<!DOCTYPE node PUBLIC
  "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
  "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">

<node>
  <interface name="org.freedesktop.portal.PhotoPicker">

    <method name="PickPhotos">
      <arg name="parent_window" type="s"    direction="in"/>
      <arg name="options"       type="a{sv}" direction="in"/>
      <arg name="handle"        type="o"    direction="out"/>
    </method>

    <signal name="Response">
      <arg name="response" type="u"/>
      <arg name="results"  type="a{sv}"/>
    </signal>

    <property name="version" type="u" access="read"/>

  </interface>
</node>
```

---

## Request Handle Lifecycle

The request handle returned by `PickPhotos` is a D-Bus object path. The portal creates a short-lived object at this path for the duration of the picker interaction. Callers must:

1. Subscribe to the `Response` signal on the handle object **before** returning from `PickPhotos` to avoid a race condition.
2. Unsubscribe and release the handle after receiving `Response`.
3. Not hold handles indefinitely — backends may time out idle pickers.

The handle path format follows the existing XDG portal convention:

```
/org/freedesktop/portal/desktop/request/{sender_id}/{token}
```

Where `{sender_id}` is the caller's unique D-Bus name with dots replaced by underscores, and `{token}` is the `handle_token` value from the options dict (or an auto-generated token if not supplied).

---

## Backend Requirements

Desktop environments implementing this portal must provide a backend service that:

1. Implements `org.freedesktop.impl.portal.PhotoPicker` at `/org/freedesktop/portal/desktop`
2. Presents the host photo library's native picker UI in response to `PickPhotos`
3. Exports selected photos to a temporary directory readable by the calling application
4. Emits `Response` with exported `file://` URIs once the user confirms or cancels
5. Cleans up temporary export files after a reasonable period (suggested: 1 hour, or when the calling application exits)
6. Respects the `parent_window` hint for dialog parenting where the window system permits

Backend implementations are encouraged to return the `metadata` array where their photo library supports it, but it is not required for a conformant implementation.

---

## Security Considerations

**Sandboxed callers (Flatpak):** The portal automatically grants the sandboxed application read access to the exported temporary files via the document portal (`org.freedesktop.portal.Documents`). No additional filesystem permissions are required.

**Unsandboxed callers:** The exported files are written to a temporary directory. The backend should use a per-session directory with permissions restricted to the calling user (`0700`).

**No library enumeration:** The interface intentionally provides no method to list or search the photo library. The user must interact with the picker UI to make selections. This prevents calling applications from silently extracting library contents.

**Sender verification:** The backend must verify that `Response` signals are delivered only to the original requesting application, matched by the sender's unique bus name encoded in the handle path.

**Metadata stripping:** When `strip_metadata` is `true`, the backend must strip all EXIF, XMP, and IPTC metadata from exported files, including GPS coordinates, before delivering them.

---

## Relation to Existing Portals

| Portal | Relationship |
|---|---|
| `org.freedesktop.portal.FileChooser` | Complementary. FileChooser operates on raw filesystem files; PhotoPicker operates on library-managed photos with album context and rich metadata. |
| `org.freedesktop.portal.Documents` | Used internally by PhotoPicker to grant sandboxed apps access to exported temporary files. Callers do not need to use Documents directly. |
| `org.freedesktop.portal.Print` | **Unrelated.** The Print portal sends documents to local or network printers. Commercial photo print services — the primary use case for this portal — communicate with their own vendor APIs over HTTPS after receiving exported file paths. The two portals serve entirely different ends of different workflows. |
| `org.freedesktop.portal.Camera` | Unrelated. Camera provides access to live camera feeds (webcams, capture devices), not stored photo libraries. |
| `org.freedesktop.portal.OpenURI` | Unrelated. |

---

## Reference Implementation

The `com.moments.Photos.Portal` interface shipped in the Moments photo application provides the reference implementation from which this proposal is derived. The Moments implementation:

- Presents Moments' native photo picker UI
- Exports selected photos to `/tmp/moments-portal/{session}/`
- Supports all option and metadata keys described in this document
- Has been in production use since Moments version X.X

The Moments implementation is intentionally a strict subset of this proposal's interface, ensuring that applications written against `com.moments.Photos.Portal` require no changes to migrate to `org.freedesktop.portal.PhotoPicker` when adopted.

---

## Example: Calling the Portal (Python)

```python
import dbus
import dbus.mainloop.glib
from gi.repository import GLib

dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)

bus       = dbus.SessionBus()
loop      = GLib.MainLoop()
portal    = bus.get_object("org.freedesktop.portal.Desktop",
                            "/org/freedesktop/portal/desktop")
iface     = dbus.Interface(portal, "org.freedesktop.portal.PhotoPicker")

options = {
    "handle_token":   "myapp_pick_1",
    "title":          "Select photos to print",
    "max_count":      dbus.UInt32(50),
    "export_format":  "jpeg",
    "export_quality": dbus.UInt32(95),
    "max_dimension":  dbus.UInt32(4000),
    "strip_metadata": False,
    "colour_profile": "sRGB",
}

handle = iface.PickPhotos("", options)

def on_response(response, results):
    if response == 0:
        uris = results.get("uris", [])
        print(f"User selected {len(uris)} photo(s):")
        for uri in uris:
            print(f"  {uri}")
    elif response == 1:
        print("User cancelled.")
    else:
        print(f"Error: {results.get('error', 'unknown')}")
    loop.quit()

request_obj = bus.get_object("org.freedesktop.portal.Desktop", handle)
request_obj.connect_to_signal("Response", on_response,
    dbus_interface="org.freedesktop.portal.Request")

loop.run()
```

---

## Example: Calling the Portal (C, using sd-bus)

```c
#include <systemd/sd-bus.h>
#include <stdio.h>

static int on_response(sd_bus_message *m, void *userdata, sd_bus_error *err) {
    uint32_t response;
    sd_bus_message_read(m, "u", &response);
    if (response == 0) {
        /* enter results variant and read uris array */
        printf("Selection confirmed.\n");
    } else {
        printf("Picker cancelled or failed.\n");
    }
    return 0;
}

int main(void) {
    sd_bus *bus = NULL;
    sd_bus_open_user(&bus);

    sd_bus_call_method_async(
        bus, NULL,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.PhotoPicker",
        "PickPhotos",
        NULL, NULL,
        "sa{sv}",
        "",              /* parent_window */
        3,               /* options count */
        "handle_token",  "s", "myapp_1",
        "max_count",     "u", (uint32_t)20,
        "export_format", "s", "jpeg"
    );

    /* match on Response signal at handle path, then run event loop */
    sd_bus_flush_close_unref(bus);
    return 0;
}
```

---

## Open Questions for XDG Review

1. **Video support:** Should a future version extend this to video clips, or should that be a separate `org.freedesktop.portal.MediaPicker` interface?
2. **Live albums / smart albums:** Should `album_hint` support dynamic/smart album identifiers, or only static user-created albums?
3. **Batch export progress:** For large selections, should the portal emit progress signals between request and response?
4. **Thumbnail pre-flight:** Should callers be able to request thumbnails before full export, to show a preview in their own UI without waiting for full export?

---

## Changelog

| Revision | Date | Notes |
|---|---|---|
| 1 | 2026-04-13 | Initial draft proposal |
| 2 | 2026-04-13 | Clarified scope boundary; portal is a source interface only. Clarified that Print portal is unrelated. Added Camera portal to relation table. |

---

*This proposal is intended for submission to the xdg-desktop-portal project at https://github.com/flatpak/xdg-desktop-portal. Feedback and co-implementors welcome.*
