# Device-Direct Label Printing (bridge side) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** BamDude Bridge asks the server for label work and prints it on a Niimbot attached to the USB port.

**Architecture:** A polling loop over the server's HTTP contract, and a port of the Niimbot wire protocol from `MultiMote/niimbluelib`. The bridge renders nothing and decides nothing about content — it receives a 1-bit PNG plus a copy count, packs it into wire rows, and reports what happened. Protocol, encoder and transport are three separate modules because only the last one needs hardware to test.

**Tech Stack:** Rust 2021 · Tauri 2 · `reqwest` (rustls, already present) · `tokio` (already present) · `serialport` (new) · `image` (new)

**Spec:** `docs/specs/2026-08-19-device-direct-labels-design.md` — read it first. The server contract it consumes is specified in the `bamdude` repository at `docs/superpowers/specs/2026-08-19-device-direct-labels-server-design.md`, which is not tracked there; if you cannot see it, ask before guessing at the wire format.

## Global Constraints

- **The server side must exist first.** Tasks 5 and 6 cannot be verified until `POST /api/v1/label-devices/poll` answers. Tasks 1–4 are hardware/protocol work and can proceed in parallel with it.
- **This is a port, not an original.** Every protocol module names the `niimbluelib` file it came from in its module doc. The remaining six print-task variants will be brought over from the same place, by someone who is not you, and a port that hides its origin is one they cannot continue.
- **CI is `windows-latest` only, and stays that way.** A Linux runner would report green on `cfg(windows)` code that never compiled. Adding a Linux job to "also check the protocol" is exactly the mistake the existing rule prevents.
- **No hardware in CI.** Every protocol and encoder test runs against fixed vectors. Device verification is a manual checklist in `CONTRIBUTING.md`.
- **Never bump `panic = "abort"` or the size-oriented release profile** to make something easier. This is a binary people download.
- **`npm run typecheck`** for the frontend half; `cargo clippy -- -D warnings` and `cargo fmt --check` for Rust.
- Branching mirrors BamDude and is enforced by workflow: work in `feature/*`, merge to `dev`, fast-forward `main` for a stable release.
- Conventional Commits.

---

### Task 1: Packet framing

**Files:**
- Create: `src-tauri/src/label/mod.rs`
- Create: `src-tauri/src/label/packet.rs`
- Modify: `src-tauri/src/lib.rs` (add `pub mod label;`)

**Interfaces:**
- Produces:
  ```rust
  pub struct Packet { pub command: u8, pub data: Vec<u8> }
  impl Packet {
      pub fn new(command: u8, data: Vec<u8>) -> Self;
      pub fn to_bytes(&self) -> Vec<u8>;
      pub fn parse(buf: &[u8]) -> Result<(Packet, usize), PacketError>;
  }
  pub enum PacketError { TooShort, BadHead, BadTail, BadChecksum }
  ```

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/label/packet.rs`, at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_is_head_command_length_data_checksum_tail() {
        let bytes = Packet::new(0xdc, vec![0x01]).to_bytes();
        assert_eq!(bytes, vec![0x55, 0x55, 0xdc, 0x01, 0x01, 0xdc ^ 0x01 ^ 0x01, 0xaa, 0xaa]);
    }

    #[test]
    fn the_checksum_covers_command_length_and_data_but_not_the_frame() {
        let bytes = Packet::new(0x01, vec![0x0a, 0x0b]).to_bytes();
        let expected = 0x01u8 ^ 0x02 ^ 0x0a ^ 0x0b;
        assert_eq!(bytes[bytes.len() - 3], expected);
    }

    #[test]
    fn parse_round_trips_what_to_bytes_produced() {
        let original = Packet::new(0x13, vec![1, 2, 3, 4, 5, 6]);
        let (parsed, consumed) = Packet::parse(&original.to_bytes()).unwrap();
        assert_eq!(parsed.command, original.command);
        assert_eq!(parsed.data, original.data);
        assert_eq!(consumed, original.to_bytes().len());
    }

    #[test]
    fn parse_reports_how_much_it_consumed_so_a_stream_can_continue() {
        let mut buf = Packet::new(0xdc, vec![1]).to_bytes();
        buf.extend(Packet::new(0x1a, vec![2]).to_bytes());
        let (first, consumed) = Packet::parse(&buf).unwrap();
        assert_eq!(first.command, 0xdc);
        let (second, _) = Packet::parse(&buf[consumed..]).unwrap();
        assert_eq!(second.command, 0x1a);
    }

    #[test]
    fn a_corrupted_checksum_is_rejected_rather_than_believed() {
        let mut bytes = Packet::new(0xdc, vec![1]).to_bytes();
        let last_data = bytes.len() - 3;
        bytes[last_data] ^= 0xff;
        assert!(matches!(Packet::parse(&bytes), Err(PacketError::BadChecksum)));
    }

    #[test]
    fn a_truncated_frame_asks_for_more_instead_of_failing() {
        let bytes = Packet::new(0xdc, vec![1, 2, 3]).to_bytes();
        assert!(matches!(Packet::parse(&bytes[..5]), Err(PacketError::TooShort)));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd src-tauri && cargo test label::packet`
Expected: compile error — `Packet` not found.

- [ ] **Step 3: Implement**

```rust
//! Niimbot packet framing.
//!
//! Ported from `niimbluelib`, `src/packets/packet.ts`. The frame is identical
//! across every model and protocol version; only the *contents* of the
//! start-of-print and page-size commands differ by generation, which is why the
//! model-specific part lives in `task.rs` and not here.

const HEAD: [u8; 2] = [0x55, 0x55];
const TAIL: [u8; 2] = [0xaa, 0xaa];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub command: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PacketError {
    #[error("need more bytes")]
    TooShort,
    #[error("frame does not start with 55 55")]
    BadHead,
    #[error("frame does not end with aa aa")]
    BadTail,
    #[error("checksum mismatch")]
    BadChecksum,
}

impl Packet {
    pub fn new(command: u8, data: Vec<u8>) -> Self {
        Self { command, data }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(2 + self.data.len());
        payload.push(self.command);
        payload.push(self.data.len() as u8);
        payload.extend_from_slice(&self.data);
        let checksum = payload.iter().fold(0u8, |acc, b| acc ^ b);

        let mut out = Vec::with_capacity(HEAD.len() + payload.len() + 1 + TAIL.len());
        out.extend_from_slice(&HEAD);
        out.extend_from_slice(&payload);
        out.push(checksum);
        out.extend_from_slice(&TAIL);
        out
    }

    /// Parse one frame from the front of `buf`, returning it and the number of
    /// bytes consumed so a caller reading a stream can continue.
    pub fn parse(buf: &[u8]) -> Result<(Packet, usize), PacketError> {
        if buf.len() < 7 {
            return Err(PacketError::TooShort);
        }
        if buf[0..2] != HEAD {
            return Err(PacketError::BadHead);
        }
        let command = buf[2];
        let len = buf[3] as usize;
        let total = 2 + 2 + len + 1 + 2;
        if buf.len() < total {
            return Err(PacketError::TooShort);
        }
        let data = buf[4..4 + len].to_vec();
        let checksum = buf[4 + len];
        let expected = buf[2..4 + len].iter().fold(0u8, |acc, b| acc ^ b);
        if checksum != expected {
            return Err(PacketError::BadChecksum);
        }
        if buf[total - 2..total] != TAIL {
            return Err(PacketError::BadTail);
        }
        Ok((Packet { command, data }, total))
    }
}
```

Create `src-tauri/src/label/mod.rs` with `pub mod packet;` and add `pub mod label;` to `lib.rs`.

- [ ] **Step 4: Run to verify it passes**

Run: `cd src-tauri && cargo test label::packet`
Expected: `test result: ok. 6 passed`

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/label/ src-tauri/src/lib.rs
git commit -m "feat(label): Niimbot packet framing, ported from niimbluelib"
```

---

### Task 2: The row encoder

The piece most likely to be subtly wrong and the one that needs no printer to prove.

**Files:**
- Create: `src-tauri/src/label/encoder.rs`
- Modify: `src-tauri/src/label/mod.rs`
- Modify: `src-tauri/Cargo.toml` (add `image = { version = "0.25", default-features = false, features = ["png"] }`)

**Interfaces:**
- Produces:
  ```rust
  pub struct EncodedImage { pub rows: u16, pub cols: u16, pub packets: Vec<Packet> }
  pub fn encode_png(png: &[u8]) -> Result<EncodedImage, EncodeError>;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(width: u32, height: u32, black: &[(u32, u32)]) -> Vec<u8> {
        let mut img = image::GrayImage::from_pixel(width, height, image::Luma([255u8]));
        for (x, y) in black {
            img.put_pixel(*x, *y, image::Luma([0u8]));
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn an_all_white_image_sends_no_bitmap_rows() {
        let enc = encode_png(&png_of(64, 4, &[])).unwrap();
        assert_eq!(enc.cols, 64);
        assert_eq!(enc.rows, 4);
        assert!(enc.packets.iter().all(|p| p.command != CMD_BITMAP_ROW));
    }

    #[test]
    fn identical_consecutive_rows_collapse_into_one_repeat() {
        // Four identical blank rows must not become four packets.
        let enc = encode_png(&png_of(64, 4, &[])).unwrap();
        assert_eq!(enc.packets.iter().filter(|p| p.command == CMD_EMPTY_ROW).count(), 1);
    }

    #[test]
    fn a_row_with_few_black_pixels_is_sent_as_indexes_not_a_bitmap() {
        let enc = encode_png(&png_of(64, 1, &[(3, 0), (17, 0)])).unwrap();
        assert!(enc.packets.iter().any(|p| p.command == CMD_BITMAP_ROW_INDEXED));
    }

    #[test]
    fn indexes_are_sixteen_bit_big_endian() {
        let enc = encode_png(&png_of(64, 1, &[(258, 0)][..0])).unwrap();
        let _ = enc; // placeholder guard; the real assertion is below
        let enc = encode_png(&png_of(512, 1, &[(258, 0)])).unwrap();
        let p = enc
            .packets
            .iter()
            .find(|p| p.command == CMD_BITMAP_ROW_INDEXED)
            .expect("indexed row");
        assert!(p.data.windows(2).any(|w| w == [0x01, 0x02]));
    }

    #[test]
    fn a_dense_row_is_sent_as_a_bitmap_msb_first() {
        // Black at x=0 must set the most significant bit of the first byte.
        let black: Vec<(u32, u32)> = (0..40).map(|x| (x, 0u32)).collect();
        let enc = encode_png(&png_of(64, 1, &black)).unwrap();
        let p = enc
            .packets
            .iter()
            .find(|p| p.command == CMD_BITMAP_ROW)
            .expect("bitmap row");
        let first_pixel_byte = p.data[p.data.len() - 8];
        assert_eq!(first_pixel_byte & 0b1000_0000, 0b1000_0000);
    }

    #[test]
    fn a_width_that_is_not_a_whole_byte_is_padded_with_white() {
        let enc = encode_png(&png_of(60, 1, &[(0, 0)])).unwrap();
        assert_eq!(enc.cols, 64);
    }

    #[test]
    fn a_bookkeeping_row_is_inserted_every_two_hundred_rows() {
        let enc = encode_png(&png_of(64, 401, &[])).unwrap();
        assert_eq!(enc.packets.iter().filter(|p| p.command == CMD_CHECK_ROW).count(), 2);
    }
}
```

- [ ] **Step 2: Run to verify it fails, then implement**

Run: `cd src-tauri && cargo test label::encoder`

Port from `niimbluelib`, `src/image_encoder.ts`. Name that file in the module doc. The decisions to reproduce exactly: a pixel is black when it is not pure white; width pads up to a multiple of eight; bits are MSB-first within each byte; consecutive identical rows become one packet with a repeat count; a row whose black-pixel count is below the threshold is sent as 16-bit big-endian indexes; a bookkeeping row goes in at every 200-row boundary.

⚠️ Do not "improve" the threshold or the repeat logic. A printer that disagrees with the reference implementation will not tell you — it will print something slightly wrong, and you will look for the bug in the transport.

- [ ] **Step 3: Run to verify it passes**

Run: `cd src-tauri && cargo test label::encoder`
Expected: `test result: ok. 7 passed`

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/label/encoder.rs src-tauri/src/label/mod.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(label): pack a 1-bit image into Niimbot wire rows"
```

---

### Task 3: The print task, behind a transport trait

**Files:**
- Create: `src-tauri/src/label/task.rs`
- Create: `src-tauri/src/label/transport.rs` (the trait plus a test double)
- Modify: `src-tauri/src/label/mod.rs`

**Interfaces:**
- Produces:
  ```rust
  #[async_trait::async_trait]
  pub trait Transport: Send {
      async fn send(&mut self, packet: &Packet) -> Result<(), TransportError>;
      async fn recv(&mut self, timeout: Duration) -> Result<Packet, TransportError>;
  }
  pub struct PrintOptions { pub density: u8, pub copies: u16 }
  pub async fn print_b1<T: Transport>(t: &mut T, img: &EncodedImage, opts: &PrintOptions)
      -> Result<(), PrintError>;
  pub fn select_task(model: &str, protocol_version: Option<u8>) -> Option<TaskKind>;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_b1_flow_sends_its_commands_in_order() {
        let mut t = FakeTransport::answering_status_after(1);
        print_b1(&mut t, &tiny_image(), &PrintOptions { density: 3, copies: 1 })
            .await
            .unwrap();
        let sent: Vec<u8> = t.sent.iter().map(|p| p.command).collect();
        let head = &sent[..3];
        assert_eq!(head, &[CMD_SET_DENSITY, CMD_SET_LABEL_TYPE, CMD_PRINT_START]);
        assert_eq!(sent.last(), Some(&CMD_PRINT_END));
        let page_start = sent.iter().position(|c| *c == CMD_PAGE_START).unwrap();
        let page_size = sent.iter().position(|c| *c == CMD_SET_PAGE_SIZE).unwrap();
        assert!(page_start < page_size, "page size follows page start");
    }

    #[tokio::test]
    async fn printing_waits_for_the_printer_to_report_the_page_finished() {
        let mut t = FakeTransport::answering_status_after(3);
        print_b1(&mut t, &tiny_image(), &PrintOptions { density: 3, copies: 1 })
            .await
            .unwrap();
        assert!(t.status_polls >= 3);
    }

    #[tokio::test]
    async fn a_transport_that_stops_answering_fails_rather_than_hanging() {
        let mut t = FakeTransport::silent();
        let err = print_b1(&mut t, &tiny_image(), &PrintOptions { density: 3, copies: 1 })
            .await
            .unwrap_err();
        assert!(matches!(err, PrintError::Transport(_)));
    }

    #[test]
    fn a_model_with_a_protocol_version_beats_the_bare_model() {
        // The reference resolves (model, version) first and falls back to model.
        assert_eq!(select_task("D110_M", Some(4)), Some(TaskKind::D110MV4));
        assert_eq!(select_task("D110_M", None), Some(TaskKind::B1));
    }

    #[test]
    fn the_b1_task_covers_more_than_the_b1() {
        for model in ["B1", "D110_M", "B21_C2B", "M2_H", "N1", "D101"] {
            assert_eq!(select_task(model, None), Some(TaskKind::B1), "{model}");
        }
    }

    #[test]
    fn an_unported_model_is_refused_rather_than_guessed_at() {
        assert_eq!(select_task("D11", None), None);
        assert_eq!(select_task("H1S", None), None);
    }
}
```

- [ ] **Step 2: Run to verify it fails, then implement**

`select_task` returns `None` for every variant not yet ported. ⚠️ **Refusing is the feature.** A model routed to the wrong flow prints garbage or jams, and the operator has no way to know which of the two happened; "unsupported model" is information they can act on.

Add `async-trait` to `Cargo.toml`.

- [ ] **Step 3: Run to verify it passes, lint, commit**

```bash
cd src-tauri && cargo test label:: && cargo clippy -- -D warnings && cargo fmt --check
git add src-tauri/src/label/ src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(label): the B1 print flow, with unported models refused rather than guessed"
```

---

### Task 4: Serial transport

**Files:**
- Create: `src-tauri/src/label/serial.rs`
- Modify: `src-tauri/Cargo.toml` (add `serialport = "4"`)
- Modify: `src-tauri/src/label/mod.rs`

**Interfaces:**
- Produces: `pub fn list_ports() -> Vec<PortInfo>`; `pub struct SerialTransport` implementing `Transport`; `#[tauri::command] pub fn label_list_ports() -> Vec<PortInfo>`.

- [ ] **Step 1: Write what can be tested without hardware**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_ports_does_not_panic_on_a_machine_with_none() {
        let _ = list_ports();
    }

    #[test]
    fn a_usb_serial_port_is_ranked_above_a_bluetooth_one() {
        let ranked = rank_candidates(vec![
            PortInfo { name: "COM5".into(), description: "Standard Serial over Bluetooth link".into() },
            PortInfo { name: "COM3".into(), description: "USB-SERIAL CH340".into() },
        ]);
        assert_eq!(ranked[0].name, "COM3");
    }
}
```

⚠️ Everything past opening a port is a manual check. Add it to `CONTRIBUTING.md` under the device checklist rather than pretending a unit test covers it.

- [ ] **Step 2: Implement, then verify on hardware**

Manual: plug a B1 in, run `cargo run -- --list-ports`, confirm the port appears and is ranked first.

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/label/serial.rs src-tauri/Cargo.toml src-tauri/Cargo.lock CONTRIBUTING.md
git commit -m "feat(label): talk to a Niimbot over USB serial"
```

---

### Task 5: The poller

**Files:**
- Create: `src-tauri/src/label/poller.rs`
- Create: `src-tauri/src/label/api.rs`
- Modify: `src-tauri/src/config.rs` (new settings)
- Modify: `src-tauri/src/lib.rs` (start the task)
- Modify: `src-tauri/Cargo.toml` (add `uuid = { version = "1", features = ["v4"] }`, `base64 = "0.22"`)

**Interfaces:**
- Produces: `pub async fn run(app: AppHandle)`; `pub struct DeviceReport`; `pub enum PollOutcome { Job(LabelJob), Idle, Disabled }`.

- [ ] **Step 1: Write the failing tests against a stub server**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_204_means_loop_again_without_touching_the_printer() {
        let server = stub_answering(204, "");
        assert!(matches!(poll_once(&server.url(), "key", &report()).await.unwrap(), PollOutcome::Idle));
    }

    #[tokio::test]
    async fn a_409_means_the_feature_is_off_and_we_back_off_hard() {
        let server = stub_answering(409, r#"{"detail":"device_labels_disabled"}"#);
        assert!(matches!(poll_once(&server.url(), "key", &report()).await.unwrap(), PollOutcome::Disabled));
        assert!(backoff_for(&PollOutcome::Disabled) >= Duration::from_secs(60));
    }

    #[tokio::test]
    async fn a_200_carries_a_decodable_png() {
        let server = stub_answering(200, &job_json_with_png());
        let PollOutcome::Job(job) = poll_once(&server.url(), "key", &report()).await.unwrap() else {
            panic!("expected a job");
        };
        assert!(image::load_from_memory(&job.image_png).is_ok());
    }

    #[tokio::test]
    async fn an_unreachable_printer_is_reported_rather_than_hidden() {
        // "bridge alive, printer gone" is the difference between "your USB
        // cable" and "your server".
        let r = build_report(None);
        assert_eq!(r.printer_reachable, false);
        assert!(r.model.is_none());
    }

    #[tokio::test]
    async fn a_result_is_posted_exactly_once_per_job() {
        let server = counting_stub();
        run_one_cycle(&server).await;
        assert_eq!(server.result_calls(), 1);
    }

    #[tokio::test]
    async fn a_network_error_backs_off_and_does_not_give_up() {
        let mut delay = Duration::ZERO;
        for _ in 0..5 {
            delay = next_backoff(delay);
        }
        assert!(delay > Duration::from_secs(1));
        assert!(delay <= MAX_BACKOFF);
    }
}
```

- [ ] **Step 2: Run to verify it fails, then implement**

The cycle: read device state (or report `printer_reachable: false`) → `POST /api/v1/label-devices/poll` → on `200`, decode, encode, print, `POST …/jobs/{id}/result` → on `204`, loop → on `409`, long back-off → on transport error, exponential back-off to a ceiling with the tray tooltip updated.

⚠️ `installation_id` is generated **once**, on first use, and stored in `settings.json`. It is what identifies the device row on the server; regenerating it orphans the paired device and creates a second one waiting for approval. There must be exactly one code path that can write it, and it must refuse to overwrite a non-empty value.

- [ ] **Step 3: Run tests, lint, commit**

```bash
cd src-tauri && cargo test label:: && cargo clippy -- -D warnings
git add src-tauri/src/label/ src-tauri/src/config.rs src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(label): ask BamDude for label work and print what comes back"
```

---

### Task 6: Window, settings, and untangling autostart

**Files:**
- Modify: `src/App.tsx`
- Modify: `src-tauri/src/registry.rs`
- Modify: `README.md`
- Modify: `CONTRIBUTING.md`

- [ ] **Step 1: Split autostart out of protocol registration**

Today `install_protocol_handler` also installs the autostart entry, and `remove_protocol_handler` removes it. Make autostart its own pair of functions, called when **either** role is switched on, and removed only when **both** are off.

Without this, an operator who wants labels and not the BambuStudio integration gets a poller that runs only while they happen to have the window open — which is indistinguishable from a broken feature.

Test what can be tested without touching the real registry: that the "should autostart be installed" decision is a pure function of the two role flags, and that it is true for (on, off), (off, on) and (on, on), false only for (off, off).

- [ ] **Step 2: Add the second tab**

Transport, port (from `list_ports`, not typed by hand), density, a **Test print**, and the state as the bridge currently sees it. Test print separates "the printer works" from "the queue works" — the two things anybody debugging this needs told apart.

- [ ] **Step 3: Document**

`README.md` gains a Label printing section. Say plainly that **closing the window hides it and the poller keeps running**, and that quitting from the tray stops it — "I closed it and labels stopped" is otherwise a support question waiting to happen. Repeat the elevation warning: run as administrator and the slicer handover breaks silently.

`CONTRIBUTING.md` gains the manual device checklist: port appears in the list; test print comes out; a job queued in BamDude prints; unplugging the printer shows as unreachable in BamDude within one poll; killing the bridge mid-job leaves a job that returns to the queue on the server's sweep.

- [ ] **Step 4: Verify and commit**

```bash
npm run typecheck && cd src-tauri && cargo clippy -- -D warnings && cargo fmt --check && cargo test
git add src/ src-tauri/src/registry.rs README.md CONTRIBUTING.md
git commit -m "feat(label): a settings tab for the printer, and autostart that serves both roles"
```

---

## Self-Review

**Spec coverage.** Component 1 (protocol port) → Tasks 1 and 3; Component 2 (encoder) → Task 2; Component 3 (serial) → Task 4; Component 4 (poller) → Task 5; Components 5 and 6 (settings, window, autostart) → Task 6. The spec's exclusions — BLE, the other six print tasks, cassette resolution, rendering, macOS — have no tasks, correctly.

**Placeholders.** Tasks 2, 4, 5 and 6 give test code and describe the implementation in prose rather than pasting it. That is deliberate for two of them and worth stating: Task 2 is a line-by-line port whose source is named, and pasting a paraphrase of it into the plan would create a second, worse reference for the next person to follow. Tasks 4 and 6 are Windows API and UI work where the tests are the specification and the code is mechanical.

**Type consistency.** `Packet` (Task 1) is what `EncodedImage.packets` holds (Task 2) and what `Transport::send` takes (Task 3). `EncodedImage` is what `print_b1` consumes. `PrintOptions { density, copies }` matches the `density` and `copies` fields the server's poll response carries. `select_task` returns `Option<TaskKind>` in both its definition and every test.

**One gap found and closed while reviewing:** the spec says an unplugged printer still polls with `printer_reachable: false`, and the first draft had no test for it. `an_unreachable_printer_is_reported_rather_than_hidden` was added to Task 5.
