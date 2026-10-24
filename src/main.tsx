import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { installLogging } from "./lib/log";
import "./index.css";

installLogging();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
