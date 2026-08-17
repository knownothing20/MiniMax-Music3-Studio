import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import "./style.css";

const serverUrl = "http://127.0.0.1:8765";

type Health = {
  status: string;
  runtime: string;
};

function App() {
  const [health, setHealth] = useState<Health | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    fetch(`${serverUrl}/health`)
      .then(async (response) => {
        if (!response.ok) throw new Error(`Server returned ${response.status}`);
        return (await response.json()) as Health;
      })
      .then((result) => active && setHealth(result))
      .catch((reason: unknown) => active && setError(String(reason)));

    return () => {
      active = false;
    };
  }, []);

  return (
    <main>
      <p className="eyebrow">NATIVE MUSIC WORKSPACE</p>
      <h1>MiniMax Music3 Studio</h1>
      <p className="description">
        The desktop shell and native service are ready. Provider, model, and execution-mode controls will be supplied by the capability registry.
      </p>
      <section aria-live="polite">
        {health && <span className="ready">Service ready · {health.runtime}</span>}
        {error && <span className="error">Service unavailable · {error}</span>}
        {!health && !error && <span className="pending">Starting local service…</span>}
      </section>
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
