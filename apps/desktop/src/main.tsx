import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { inTauri } from "./transport";
import "./styles.css";

// Inside Tauri the OS window provides the glass material — the painted
// gradient is a browser-only fallback for `pnpm dev`.
if (inTauri()) document.body.classList.add("glass");

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
