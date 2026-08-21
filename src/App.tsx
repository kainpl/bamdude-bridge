import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Settings {
  server_url: string;
  api_key: string;
  label_enabled: boolean;
  label_port: string;
}

/** Mirrors `label::serial::PortInfo`. */
interface PortInfo {
  name: string;
  description: string;
  usb: boolean;
}

/** Mirrors `update::UpdateCheck`. */
interface UpdateCheck {
  available: boolean;
  current_version: string;
  version: string | null;
  /** The GitHub release body. ⚠️ Shown verbatim — it is the real notes, not a
   *  second changelog somebody has to remember to keep. */
  notes: string | null;
  date: string | null;
  /** True when this copy replaces its own executable instead of running an
   *  installer. The two feel different enough to say so. */
  portable: boolean;
}

/** Mirrors `label::poller::PollerStatus`. */
interface PollerStatus {
  installation_id: string;
  last_contact: string | null;
  last_outcome: string | null;
  idle: boolean;
}

/** Mirrors `label::commands::PortsResult`. */
interface PortsResult {
  ports: PortInfo[];
  note: string;
}

/** Mirrors `label::status::Heartbeat`. Every field is optional because a
 *  printer that answers one question and not another is normal, and showing it
 *  half-filled is more use than refusing the lot. */
interface Heartbeat {
  lid_closed: boolean | null;
  charge_level: number | null;
  paper_inserted: boolean | null;
  tag_read: boolean | null;
}

/** Mirrors `label::status::Cassette`. Note what is absent: no size in
 *  millimetres, because the tag does not carry one. */
interface Cassette {
  uuid: string;
  barcode: string;
  serial: string;
  total: number;
  used: number;
  consumable_type: number;
  consumable_name: string;
  capacity: number | null;
}

/** Mirrors `label::status::PrinterSnapshot`. */
interface PrinterSnapshot {
  model_id: number | null;
  model_name: string | null;
  supported: boolean;
  dpi: number | null;
  printhead_pixels: number | null;
  density_min: number | null;
  density_max: number | null;
  density_default: number | null;
  firmware: string | null;
  serial: string | null;
  heartbeat: Heartbeat | null;
  cassette: Cassette | null;
}

/** Mirrors `HandoverStatus` in lib.rs — kept in the same shape deliberately. */
type Handover =
  | { state: "started"; name: string }
  | { state: "succeeded"; name: string }
  | { state: "failed"; name: string; error: string };

/** Mirrors `registry::Owner`. */
type Owner =
  | { owner: "nobody" }
  | { owner: "us" }
  | { owner: "foreign"; command: string; machine_wide: boolean };

/** Mirrors `registry::Status`. */
interface Registration {
  marker_present: boolean;
  protocol: Owner;
  autostart: boolean;
  elevated: boolean;
}

/** The two roles, which share only a server address and a window. */
type Tab = "files" | "labels" | "updates";

const EMPTY: Settings = {
  server_url: "",
  api_key: "",
  label_enabled: false,
  label_port: "",
};

export function App() {
  const [settings, setSettings] = useState<Settings>(EMPTY);
  const [registration, setRegistration] = useState<Registration | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);
  const [handover, setHandover] = useState<Handover | null>(null);
  const [version, setVersion] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("files");
  // Read from the cache the background checker fills, so the dot appears on
  // its own — the whole point of checking on a schedule is not having to ask.
  const [updateReady, setUpdateReady] = useState(false);
  useEffect(() => {
    const read = () => {
      invoke<UpdateCheck | null>("last_update_check")
        .then((last) => setUpdateReady(Boolean(last?.available)))
        .catch(() => setUpdateReady(false));
    };
    read();
    const timer = window.setInterval(read, 60_000);
    return () => window.clearInterval(timer);
  }, []);

  const refreshRegistration = useCallback(() => {
    invoke<Registration>("registration_status").then(setRegistration).catch(() => {
      // Not fatal and not worth a banner: on a non-Windows build the command
      // does not exist, and the receiver section simply stays hidden.
      setRegistration(null);
    });
  }, []);

  useEffect(() => {
    invoke<Settings>("load_settings")
      .then(setSettings)
      .catch((error: unknown) => setMessage({ tone: "bad", text: String(error) }));
    invoke<string>("app_version").then(setVersion).catch(() => setVersion(null));
    refreshRegistration();
  }, [refreshRegistration]);

  // Two halves, and both are needed.
  //
  // The subscription catches a handover that lands while this window is
  // already open — the slicer sends a second plate and single-instance routes
  // it here.
  //
  // ⚠️ The fetch catches the one that finished BEFORE this component mounted.
  // A handover that starts the process reports from Rust's setup(), well
  // before React is listening, so relying on the event alone loses exactly the
  // case that matters most — and an empty window reads as success.
  useEffect(() => {
    invoke<Handover | null>("last_handover")
      .then((last) => {
        if (last) setHandover(last);
      })
      .catch(() => {
        // Nothing to show is a normal first run, not an error worth a banner.
      });

    const stop = listen<Handover>("handover", (event) => setHandover(event.payload));
    return () => {
      void stop.then((unlisten) => unlisten());
    };
  }, []);

  async function run(action: () => Promise<string | void>) {
    setBusy(true);
    setMessage(null);
    try {
      const text = await action();
      setMessage({ tone: "ok", text: text || "Saved." });
    } catch (error: unknown) {
      setMessage({ tone: "bad", text: String(error) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <main>
      <header>
        <h1>BamDude Bridge</h1>
        {/* Shown next to the name rather than hidden in an About box: the
            first question about any misbehaving build is which one it is. */}
        {version && <span className="version">v{version}</span>}
      </header>

      {/* Above everything, because nothing else in this window works while it
          is true — and the failure it causes leaves no trace at all. */}
      {registration?.elevated && (
        <section className="card bad">
          <strong>Running as administrator</strong>
          <br />
          BambuStudio cannot hand files to an elevated app, so anything you send from the slicer
          will quietly do nothing. Quit from the tray and start Bridge normally — registering asks
          for administrator rights on its own, only when it needs them.
        </section>
      )}

      {handover && <HandoverCard status={handover} />}

      <form
        onSubmit={(event) => {
          event.preventDefault();
          void run(() => invoke("save_settings", { settings }));
        }}
      >
        <label>
          Server address
          <input
            type="url"
            placeholder="http://192.168.1.10:8000"
            value={settings.server_url}
            onChange={(event) => setSettings({ ...settings, server_url: event.target.value })}
          />
        </label>

        <label>
          API key
          <input
            type="password"
            placeholder="bb_…"
            value={settings.api_key}
            onChange={(event) => setSettings({ ...settings, api_key: event.target.value })}
          />
          <small>
            Needs the <strong>library-manage</strong> scope, and nothing else. Create one in BamDude
            under Settings → API keys.
          </small>
        </label>

        <div className="row">
          <button type="submit" disabled={busy}>
            Save
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => void run(() => invoke<string>("test_connection", { settings }))}
          >
            Test connection
          </button>
        </div>
      </form>

      {/* Two roles, two tabs. The server address above stays outside them
          because both use it — putting it inside the slicer tab would say it
          belonged to the slicer. */}
      <nav className="tabs" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={tab === "files"}
          onClick={() => setTab("files")}
        >
          Files from BambuStudio
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "labels"}
          onClick={() => setTab("labels")}
        >
          Label printer
          {settings.label_enabled && <span className="dot" aria-label="on" />}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "updates"}
          onClick={() => setTab("updates")}
        >
          Updates
          {/* The same dot the label role uses: says there is something here
              without opening the tab to find out. */}
          {updateReady && <span className="dot" aria-label="update available" />}
        </button>
      </nav>

      {tab === "files" &&
        (registration ? (
          <ReceiverSection
            registration={registration}
            busy={busy}
            onRegister={() =>
              void run(async () => {
                setRegistration(await invoke<Registration>("register_receiver"));
                return "Registered. Restart BambuStudio to see the menu entry.";
              })
            }
            onUnregister={() =>
              void run(async () => {
                setRegistration(await invoke<Registration>("unregister_receiver"));
                return "Removed.";
              })
            }
          />
        ) : (
          <section className="panel">
            <h2>Receiving files from BambuStudio</h2>
            <small>
              Only available on Windows — Bambu never implemented the hand-off on any other system.
            </small>
          </section>
        ))}

      {tab === "labels" && (
        <LabelPrinterSection
          settings={settings}
          busy={busy}
          onChange={(next) => {
            // A toggle and a dropdown are discrete choices, so they persist the
            // moment they are made. The Save button above exists for the text
            // fields, where saving half a typed URL would be wrong.
            setSettings(next);
            void run(async () => {
              await invoke("save_settings", { settings: next });
              return "Saved.";
            });
          }}
          onMessage={setMessage}
        />
      )}

      {tab === "updates" && <UpdateSection onAvailable={setUpdateReady} />}

      {message && <p className={message.tone === "ok" ? "ok" : "bad"}>{message.text}</p>}
    </main>
  );
}

function ReceiverSection({
  registration,
  busy,
  onRegister,
  onUnregister,
}: {
  registration: Registration;
  busy: boolean;
  onRegister: () => void;
  onUnregister: () => void;
}) {
  const { marker_present: marker, protocol, autostart } = registration;
  const [confirmed, setConfirmed] = useState(false);

  // Taking the scheme from another program is the one action here that harms
  // something outside this app, so it needs an explicit tick — never a plain
  // button that silently displaces whatever was there.
  const foreign = protocol.owner === "foreign";
  const blocked = foreign && !confirmed;

  return (
    <section className="panel">
      <h2>Receiving files from BambuStudio</h2>

      <ul className="state">
        <li className={marker ? "ok" : undefined}>
          {marker
            ? "BambuStudio will offer “Send to Bambu Farm Manager Client”."
            : "BambuStudio does not show the menu entry yet."}
        </li>
        <li className={protocol.owner === "us" ? "ok" : undefined}>
          {protocol.owner === "us" && "Files sent from the slicer arrive here."}
          {protocol.owner === "nobody" && "Nothing currently receives those files."}
          {protocol.owner === "foreign" && "Another program currently receives those files."}
        </li>
        <li className={autostart ? "ok" : undefined}>
          {autostart
            ? "Starts with Windows, straight to the tray."
            : "Does not start with Windows — the first plate after a reboot waits for a cold start."}
        </li>
      </ul>

      {protocol.owner === "foreign" && (
        <div className="card bad">
          <strong>Something else is registered</strong>
          <br />
          Windows allows one handler per link type, so registering Bridge takes those files away from:
          <code>{protocol.command}</code>
          {protocol.machine_wide && (
            <>
              <br />
              It is installed for every user on this machine. Bridge registers for your account only,
              which takes precedence — removing Bridge later hands the files back.
            </>
          )}
          <label className="confirm">
            <input type="checkbox" checked={confirmed} onChange={(e) => setConfirmed(e.target.checked)} />
            I want Bridge to receive them instead
          </label>
        </div>
      )}

      <div className="row">
        <button type="button" disabled={busy || blocked} onClick={onRegister}>
          {protocol.owner === "us" ? "Re-register" : "Register Bridge as the receiver"}
        </button>
        {protocol.owner === "us" && (
          <button type="button" disabled={busy} onClick={onUnregister}>
            Stop receiving
          </button>
        )}
      </div>

      {!marker && (
        <small>
          Registering also asks for administrator rights once, to write the key BambuStudio looks for.
          The slicer reads it at startup, so restart BambuStudio afterwards.
        </small>
      )}
    </section>
  );
}

/**
 * The label-printer role: switch it on, pick the device, see what it is.
 *
 * ⚠️ There is deliberately no paper-size field and no density field here. Both
 * are inputs to the image, the image is made on the server, and a size settable
 * in two places becomes two sizes — of which the server's is the one the image
 * was actually drawn to. This section reads and reports; it does not decide.
 */
function LabelPrinterSection({
  settings,
  busy,
  onChange,
  onMessage,
}: {
  settings: Settings;
  busy: boolean;
  onChange: (next: Settings) => void;
  onMessage: (message: { tone: "ok" | "bad"; text: string } | null) => void;
}) {
  const [ports, setPorts] = useState<PortsResult | null>(null);
  const [snapshot, setSnapshot] = useState<PrinterSnapshot | null>(null);
  const [reading, setReading] = useState(false);

  const refreshPorts = useCallback(() => {
    invoke<PortsResult>("label_list_ports").then(setPorts).catch(() => setPorts(null));
  }, []);

  useEffect(() => {
    if (settings.label_enabled) refreshPorts();
  }, [settings.label_enabled, refreshPorts]);

  // The printer is forgotten whenever the port changes: what was read belongs
  // to the device that was asked, and showing it beside a different port would
  // describe the wrong machine.
  useEffect(() => setSnapshot(null), [settings.label_port]);

  async function readPrinter() {
    setReading(true);
    onMessage(null);
    try {
      setSnapshot(await invoke<PrinterSnapshot>("label_read_status", { port: settings.label_port }));
    } catch (error: unknown) {
      setSnapshot(null);
      onMessage({ tone: "bad", text: String(error) });
    } finally {
      setReading(false);
    }
  }

  async function testPrint() {
    setReading(true);
    onMessage(null);
    try {
      const text = await invoke<string>("label_test_print", { port: settings.label_port });
      onMessage({ tone: "ok", text });
    } catch (error: unknown) {
      onMessage({ tone: "bad", text: String(error) });
    } finally {
      setReading(false);
    }
  }

  const chosen = settings.label_port.trim() !== "";
  const working = busy || reading;

  return (
    <section className="panel">
      <h2>Label printer on this machine</h2>

      <label className="confirm">
        <input
          type="checkbox"
          checked={settings.label_enabled}
          onChange={(event) => onChange({ ...settings, label_enabled: event.target.checked })}
        />
        Print labels on a printer attached to this computer
      </label>
      <small>
        Off unless you have one. BamDude composes the label; this app puts it on the printer plugged
        in here, which a server on another machine cannot reach at all.
      </small>

      {settings.label_enabled && (
        <>
          <label>
            Serial port
            <select
              value={settings.label_port}
              onChange={(event) => onChange({ ...settings, label_port: event.target.value })}
            >
              <option value="">Choose a port…</option>
              {ports?.ports.map((port) => (
                <option key={port.name} value={port.name}>
                  {port.name} — {port.description}
                  {port.usb ? "" : " (not USB)"}
                </option>
              ))}
            </select>
            <small>
              {ports?.ports.length === 0
                ? "No serial ports found. Is the printer plugged in and switched on?"
                : "USB devices are listed first; the Bluetooth entries Windows always carries answer nothing here."}
            </small>
          </label>

          {ports?.note && <small>{ports.note}</small>}

          <div className="row">
            <button type="button" disabled={working} onClick={refreshPorts}>
              Refresh ports
            </button>
            <button type="button" disabled={working || !chosen} onClick={() => void readPrinter()}>
              Read printer
            </button>
            <button type="button" disabled={working || !chosen} onClick={() => void testPrint()}>
              Test print
            </button>
          </div>

          {snapshot && <PrinterFacts snapshot={snapshot} />}

          <PollerFacts />
        </>
      )}
    </section>
  );
}

/**
 * Whether BamDude is actually being asked for work, and under what name.
 *
 * ⚠️ The installation id is the point of this block. A device shows up in
 * BamDude's list unadopted, identified only by this string — without it on
 * screen somebody is matching a UUID against a list of UUIDs by eye.
 */
function PollerFacts() {
  const [status, setStatus] = useState<PollerStatus | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    const read = () => {
      invoke<PollerStatus>("label_poller_status").then(setStatus).catch(() => setStatus(null));
    };
    read();
    // The loop's own cadence is the server's; this is just the window keeping
    // up with it, and a second is fast enough to feel live without being work.
    const timer = window.setInterval(read, 1000);
    return () => window.clearInterval(timer);
  }, []);

  if (!status) return null;

  const copy = () => {
    void navigator.clipboard.writeText(status.installation_id);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };

  return (
    <div className="poller">
      <h3>This machine, to the server</h3>
      <ul className="state">
        <li>
          <code>{status.installation_id || "not generated yet"}</code>
          {status.installation_id && (
            <button type="button" className="link" onClick={copy}>
              {copied ? "Copied" : "Copy"}
            </button>
          )}
        </li>
        <li className={status.last_contact ? "ok" : undefined}>
          {status.last_outcome ??
            (status.idle
              ? "Waiting — fill in the server and key on the Files tab, and pick a port above."
              : "Starting up…")}
        </li>
      </ul>
      <small>
        Find this id under Settings → Label printers in BamDude and switch it on. Until somebody
        does, this machine is listed but gets no work — which is deliberate: signing in proves the
        app is ours, not that this printer should be given your labels.
      </small>
    </div>
  );
}

/**
 * Whether there is a newer version, and the button that takes it.
 *
 * ⚠️ Never checks by itself on startup. This app is started by the slicer
 * handing it a file; a version check racing that would delay the one thing the
 * launch was for. The person asks.
 */
function UpdateSection({ onAvailable }: { onAvailable: (ready: boolean) => void }) {
  const [state, setState] = useState<UpdateCheck | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [checkedAt, setCheckedAt] = useState<Date | null>(null);

  // ⚠️ Opens on whatever the scheduled check already found. Asking the
  // network here would put a spinner in front of an answer we have.
  useEffect(() => {
    invoke<UpdateCheck | null>("last_update_check")
      .then((last) => {
        if (last) setState(last);
      })
      .catch(() => {});
  }, []);

  const check = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await invoke<UpdateCheck>("check_for_update");
      setState(result);
      setCheckedAt(new Date());
      onAvailable(result.available);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const install = async () => {
    setBusy(true);
    setError(null);
    try {
      // Either path ends with this process gone — the installed one is exited
      // by the installer, the portable one by us so the helper can take our
      // place. A success message here would be shown to nobody.
      await invoke("install_update");
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };

  return (
    <section className="panel">
      <h2>Updates</h2>
      <small>
        BamDude Bridge looks for a new version shortly after it starts and every few hours after
        that. This button asks straight away.
      </small>

      <div className="row">
        <button type="button" onClick={() => void check()} disabled={busy}>
          Check for updates
        </button>
        {state && !state.available && !busy && (
          <span className="muted">
            Up to date ({state.current_version})
            {checkedAt && ` · checked ${checkedAt.toLocaleTimeString()}`}
          </span>
        )}
      </div>

      {error && <p className="error">{error}</p>}

      {state?.available && (
        <div className="update">
          <p>
            <strong>{state.version}</strong> is available — you have {state.current_version}.
          </p>
          {state.notes && <pre className="notes">{state.notes}</pre>}
          <div className="row">
            <button type="button" onClick={() => void install()} disabled={busy}>
              {busy ? "Installing…" : "Download and install"}
            </button>
          </div>
          <small>
            {state.portable
              ? "This is a portable copy: the new version replaces this file and starts itself. The folder has to be writable — a copy inside Program Files will refuse rather than half-apply."
              : "The installer runs with a progress bar and asks nothing. BamDude Bridge closes and comes back on the new version."}
          </small>
        </div>
      )}
    </section>
  );
}

function PrinterFacts({ snapshot }: { snapshot: PrinterSnapshot }) {
  const { heartbeat: hb, cassette } = snapshot;
  const yesNo = (value: boolean | null, yes: string, no: string) =>
    value === null ? null : value ? yes : no;

  return (
    <>
      <ul className="state">
        <li className={snapshot.supported ? "ok" : undefined}>
          {snapshot.model_name
            ? `${snapshot.model_name} — ${snapshot.dpi} dpi, ${snapshot.printhead_pixels} dots across, density ${snapshot.density_min}–${snapshot.density_max}.`
            : snapshot.model_id !== null
              ? `The printer reports model ${snapshot.model_id}, which this app cannot print on yet.`
              : "The printer did not say what model it is."}
        </li>
        {snapshot.firmware && (
          <li>
            Firmware {snapshot.firmware}
            {snapshot.serial && ` · serial ${snapshot.serial}`}
          </li>
        )}
        {hb && (
          <li className={hb.paper_inserted ? "ok" : undefined}>
            {[
              yesNo(hb.paper_inserted, "Paper loaded", "No paper"),
              yesNo(hb.lid_closed, "lid closed", "lid open"),
              hb.charge_level !== null ? `charge ${hb.charge_level}` : null,
            ]
              .filter(Boolean)
              .join(" · ")}
          </li>
        )}
      </ul>

      {cassette ? (
        <div className="card">
          <strong>Cassette {cassette.barcode}</strong>
          <br />
          {cassette.consumable_name} · {cassette.used} of {cassette.total} used
          <br />
          {/* Said out loud because it is the question everyone asks next, and
              the honest answer is that the printer genuinely does not know. */}
          <small>
            The cassette does not report its size in millimetres — no Niimbot tag does. BamDude
            resolves that from the barcode above, and asks you once for a size it has not seen.
          </small>
        </div>
      ) : (
        <div className="card">No cassette tag detected.</div>
      )}
    </>
  );
}

function HandoverCard({ status }: { status: Handover }) {
  if (status.state === "started") {
    return (
      <section className="card">
        Sending <strong>{status.name}</strong>…
      </section>
    );
  }

  if (status.state === "succeeded") {
    return (
      <section className="card ok">
        <strong>{status.name}</strong> is in your library.
      </section>
    );
  }

  return (
    <section className="card bad">
      <strong>{status.name}</strong> did not arrive.
      <br />
      {status.error}
    </section>
  );
}
