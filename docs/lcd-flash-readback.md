# LCD flash readback investigation

Status (2026-09-05): **not implemented**. Base main commit:
`6a3fad2a4d29ce90d7126190c81f8136ced8b58e`.

Startup currently decodes the application's local media cache into RAM. It does
not extract the current LCD flash contents. ON streaming and Set hardware
image/GIF remain unchanged. Do not promote the diagnostic receive sequence into
startup: neither an asset CRC nor an input event proves that it returns media.

## Static evidence

The supplied binaries were parsed as PE files and disassembled without loading
or executing them. Addresses below are RVAs for these exact files.

| File | SHA-256 |
| --- | --- |
| iD_BD_x64_cc021.dll | 8829b63f20e6de26a1013f1c2a888b7a972c7c1156cfe303b6df6ccfeee1b124 |
| TouchscreenProtocols.dll | 09798f392b0d710f12249666e9a4f2e2495e479bffbb81b7a5f37cdf6847747e |
| DeviceCommands.dll | 3657b68be2a59776d35cfaf8667c140407b8fc47d97bae0b65ff442c6d2c135c |
| iD_SDK_V306.0.0.dll | 1c18ac7be8c4834fe1283cefc23ce1f07212ffd942557856d15e90cf144c60ec |

In cc021:

- `get_asset_CRC` (0x22c0) sends `03 23 00 <asset>` and polls feature
  report 0x16. It returns a four-byte CRC from report offsets 3 through 6.
  This selects an asset for a CRC query, not an established content download.
- `receive_input_data` (0x2f00) writes `03 1f` into the shared feature buffer
  and sends 32 bytes. It has no asset, offset, or size argument. The function
  itself does not initialize the remaining bytes of that buffer.
- `receive_input_data_ack` (0x2eb0) similarly writes `03 21` and sends
  32 bytes. Neither function by itself identifies what the payload means.
- `wait_event` (0x50a0) requests a 512-byte HID input report, checks report
  ID 1, and strips that ID. The caller receives 511 bytes: event type at 0,
  a little-endian length at 1..4, and 506 payload bytes at 5..510.
  It returns 7 on timeout and 0 on success. It does not expose the actual
  HID read length to its caller, so successful return is insufficient to
  validate a complete media transfer.

In TouchscreenProtocols, the cc021 wait-event import is at IAT RVA 0x54530.
Its wrapper at 0x30410 calls it at 0x3043d. For event type 0, the wrapper
reads the first payload byte and converts it to a double in an event object.
Other types produce no event object through this wrapper. This is concrete
evidence of a scalar event consumer, not a JPEG/GIF transfer consumer.
The precise meaning of that scalar has not been established here.

TouchscreenProtocols imports cc021 animation hash and animation segment upload
operations, but not its receive-input request or ACK exports. The supplied
SDK's named device operations target BS010. DeviceCommands' exported names
include no `GetDynamicResourceBuffer` counterpart to its setter. These are
limits of the inspected evidence; they do not prove firmware lacks readback.

## Remaining proof needed

An exact vendor implementation, firmware handler, or existing USB capture must
establish how to request asset 2's contents and how the response is framed.
For animation, its stored representation and timing must also be established;
do not assume flash contains the original GIF file.

A usable implementation needs bounded transfer size/time, complete chunk
assembly, content validation, and device/session cleanup before streaming or
OFF playback starts. Only verified device media should take precedence over
the local fallback. Preserve the existing persistent Set hardware image/GIF
operation and update playback only after that operation succeeds.

The opt-in probe captures metadata and one event for investigation. Its full
event dump is the vendor-returned structure, not a raw HID packet or a proven
asset. No device experiment was performed in this investigation. Do not infer
flash-read semantics from opcode adjacency, payload size, or API names.
