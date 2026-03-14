import ReactDOM from "react-dom/client";
import "./miniplayer.css";
import MiniPlayer from "./components/MiniPlayer";

// Sync zoom level with main app
const ZOOM_KEY = "sone.zoom.v1";

function applyZoom() {
  const savedZoom = localStorage.getItem(ZOOM_KEY);
  if (savedZoom) {
    try {
      const zoom = JSON.parse(savedZoom);
      if (typeof zoom === "number" && zoom >= 0.5 && zoom <= 2.0) {
        document.documentElement.style.zoom = String(zoom);
      }
    } catch {
      // ignore malformed value
    }
  }
}

applyZoom();

// Live-update zoom when main window changes it
window.addEventListener("storage", (e) => {
  if (e.key === ZOOM_KEY) {
    applyZoom();
  }
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <MiniPlayer />,
);
