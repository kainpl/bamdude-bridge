import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Settings {
  server_url: string;
  api_key: string;
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
}

const EMPTY: Settings = { server_url: "", api_key: "" };

export function App() {
  const [settings, setSettings] = useState<Settings>(EMPTY);
  const [registration, setRegistration] = useState<Registration | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);
  const [handover, setHandover] = useState<Handover | null>(null);

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
      <h1>BamDude Bridge</h1>

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
          <small>Needs the library-manage scope. Create one in BamDude under Settings → API keys.</small>
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

      {registration && (
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
      )}

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
  const { marker_present: marker, protocol } = registration;
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
