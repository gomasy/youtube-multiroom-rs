import { createRoot } from "react-dom/client";
import { App } from "./App";
import { init } from "./i18n";
import "./styles/main.scss";

init().then(() => {
  createRoot(document.getElementById("root")!).render(<App />);
});
