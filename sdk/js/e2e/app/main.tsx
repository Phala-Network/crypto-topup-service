import { StrictMode, useState } from "react";
import { createRoot } from "react-dom/client";
import { Checkout } from "../../dist/react/index.js";

const params = new URLSearchParams(window.location.search);

function App() {
  const [events, setEvents] = useState<string[]>([]);
  return (
    <main>
      <Checkout
        clientSecret={params.get("client_secret") ?? ""}
        apiBase={params.get("api_base") ?? ""}
        pollInterval={500}
        onSuccess={() => setEvents((e) => [...e, "success"])}
        onExpire={() => setEvents((e) => [...e, "expire"])}
      />
      <p data-testid="events">{events.join(",")}</p>
    </main>
  );
}

const root = document.getElementById("root");
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
