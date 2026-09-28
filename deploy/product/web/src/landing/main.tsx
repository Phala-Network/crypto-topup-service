import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "../index.css";
import { Landing } from "./Landing.js";

const root = document.getElementById("root");
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      <Landing />
    </StrictMode>,
  );
}
