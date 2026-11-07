import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";
import { isPaletteWindow, isTauri, ops, showMainWindow } from "./lib/api";

const app = mount(App, { target: document.getElementById("app")! });

// The window starts hidden (see tauri.conf.json) so the user never sees an
// unstyled frame. Reveal it once the first paint is queued.
if (isTauri()) {
  requestAnimationFrame(async () => {
    // The palette shows itself; the main window stays hidden when launched at login
    // in tray mode (`--hidden`).
    // The palette window stays hidden until the hotkey or tray shows it.
    if (!isPaletteWindow() && (await ops.shouldShowMain())) {
      void showMainWindow();
    }
  });
}

export default app;
