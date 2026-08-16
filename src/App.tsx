import { useEffect, useState } from "react";
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

const EMPTY: Settings = { server_url: "", api_key: "" };

export function App() {
  const [settings, setSettings] = useState<Settings>(EMPTY);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);
  const [handover, setHandover] = useState<Handover | null>(null);

  useEffect(() => {
    invoke<Settings>("load_settings")
      .then(setSettings)
      .catch((error: unknown) => setMessage({ tone: "bad", text: String(error) }));
  }, []);

  // The handover can land while this window is open — the slicer sends a
  // second plate, single-instance routes it here — so this stays subscribed
  // rather than reading a one-shot value at startup.
  useEffect(() => {
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

      {message && <p className={message.tone === "ok" ? "ok" : "bad"}>{message.text}</p>}
    </main>
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
