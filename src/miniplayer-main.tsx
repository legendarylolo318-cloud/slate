import ReactDOM from "react-dom/client";
import "./miniplayer.css";

function MiniPlayerApp() {
  return (
    <div style={{ width: "100%", height: "100%", background: "#1a1a1a", color: "white", display: "flex", alignItems: "center", justifyContent: "center", borderRadius: 12 }}>
      Miniplayer placeholder
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <MiniPlayerApp />,
);
