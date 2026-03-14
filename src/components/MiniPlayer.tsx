import { useState, useEffect, useRef, useCallback, type RefObject } from "react";
import {
  Play,
  Pause,
  SkipBack,
  SkipForward,
  Heart,
  Shuffle,
  Repeat,
  X,
  Share2,
  Volume2,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useMiniplayerBridge } from "../hooks/useMiniplayerBridge";
import { getTidalImageUrl, getTrackDisplayTitle } from "../types";
import { formatTime } from "../lib/format";
import TidalImage from "./TidalImage";

// ─── Types ──────────────────────────────────────────────────────────────────

type Tier = "narrow" | "compact" | "square" | "tall";

interface VibrantColors {
  bg: string;
  overlay: string;
  textPrimary: string;
  textSecondary: string;
  textMuted: string;
  isDark: boolean;
}

// ─── useTier ────────────────────────────────────────────────────────────────

function useTier(ref: RefObject<HTMLDivElement | null>): Tier {
  const [tier, setTier] = useState<Tier>("square");

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const { width, height } = entry.contentRect;

      if (width < 260) {
        setTier("narrow");
      } else if (height < 120) {
        setTier("compact");
      } else if (height < 380) {
        setTier("square");
      } else {
        setTier("tall");
      }
    });

    observer.observe(el);
    return () => observer.disconnect();
  }, [ref]);

  return tier;
}

// ─── useVibrantColors ───────────────────────────────────────────────────────

function parseHex(hex: string): [number, number, number] | null {
  const m = hex.match(/^#?([0-9a-f]{6})$/i);
  if (!m) return null;
  const h = m[1];
  return [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)];
}

function useVibrantColors(vibrantColor?: string): VibrantColors {
  if (!vibrantColor) {
    return {
      bg: "#1a1a1a",
      overlay: "rgba(0,0,0,0.15)",
      textPrimary: "#ffffff",
      textSecondary: "rgba(255,255,255,0.7)",
      textMuted: "rgba(255,255,255,0.5)",
      isDark: true,
    };
  }

  const rgb = parseHex(vibrantColor);
  if (!rgb) {
    return {
      bg: "#1a1a1a",
      overlay: "rgba(0,0,0,0.15)",
      textPrimary: "#ffffff",
      textSecondary: "rgba(255,255,255,0.7)",
      textMuted: "rgba(255,255,255,0.5)",
      isDark: true,
    };
  }

  const [r, g, b] = rgb;
  const luminance = (Math.max(r, g, b) + Math.min(r, g, b)) / 2 / 255;
  const isDark = luminance < 0.6;

  return {
    bg: vibrantColor.startsWith("#") ? vibrantColor : `#${vibrantColor}`,
    overlay: isDark ? "rgba(0,0,0,0.15)" : "rgba(0,0,0,0.05)",
    textPrimary: isDark ? "#ffffff" : "#1a1a1a",
    textSecondary: isDark ? "rgba(255,255,255,0.7)" : "rgba(0,0,0,0.6)",
    textMuted: isDark ? "rgba(255,255,255,0.5)" : "rgba(0,0,0,0.4)",
    isDark,
  };
}

// ─── DragHandle ─────────────────────────────────────────────────────────────

function DragHandle({ horizontal, colors }: { horizontal: boolean; colors: VibrantColors }) {
  const dotColor = colors.isDark ? "rgba(255,255,255,0.35)" : "rgba(0,0,0,0.25)";
  const visibilityClass = horizontal ? "opacity-0 group-hover:opacity-100 transition-opacity" : "";

  return (
    <div
      data-tauri-drag-region
      className={`absolute top-0 left-0 right-0 z-10 flex justify-center pt-1.5 pb-1 ${visibilityClass}`}
    >
      <div data-tauri-drag-region className="grid grid-cols-3 gap-[3px]">
        {Array.from({ length: 6 }).map((_, i) => (
          <div
            key={i}
            data-tauri-drag-region
            className="w-[4px] h-[4px] rounded-full"
            style={{ backgroundColor: dotColor }}
          />
        ))}
      </div>
    </div>
  );
}

// ─── CloseButton ────────────────────────────────────────────────────────────

function CloseButton({ colors }: { colors: VibrantColors }) {
  return (
    <button
      onClick={() => getCurrentWindow().close()}
      className="absolute top-1.5 left-1.5 z-20 w-5 h-5 rounded-full flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity"
      style={{
        backgroundColor: colors.isDark ? "rgba(255,255,255,0.15)" : "rgba(0,0,0,0.1)",
        color: colors.textPrimary,
      }}
    >
      <X size={11} strokeWidth={2.5} />
    </button>
  );
}

// ─── PlayButton ─────────────────────────────────────────────────────────────

function PlayButton({
  isPlaying,
  onClick,
  size,
  colors,
}: {
  isPlaying: boolean;
  onClick: () => void;
  size: number;
  colors: VibrantColors;
}) {
  const iconSize = Math.round(size * 0.45);
  return (
    <button
      onClick={onClick}
      className="rounded-full flex items-center justify-center hover:scale-105 active:scale-95 transition-transform duration-150 flex-shrink-0"
      style={{
        width: size,
        height: size,
        backgroundColor: colors.textPrimary,
        color: colors.bg,
      }}
    >
      {isPlaying ? (
        <Pause size={iconSize} fill="currentColor" />
      ) : (
        <Play size={iconSize} fill="currentColor" style={{ marginLeft: iconSize * 0.1 }} />
      )}
    </button>
  );
}

// ─── FavoriteButton ─────────────────────────────────────────────────────────

function FavoriteButton({
  isFavorite,
  onClick,
  colors,
  size = 16,
}: {
  isFavorite: boolean;
  onClick: () => void;
  colors: VibrantColors;
  size?: number;
}) {
  return (
    <button
      onClick={onClick}
      className="flex items-center justify-center transition-[color,transform] duration-200 active:scale-90 flex-shrink-0"
      style={{
        color: isFavorite
          ? colors.isDark
            ? "#ff6b8a"
            : "#e5395f"
          : colors.textSecondary,
      }}
    >
      <Heart size={size} fill={isFavorite ? "currentColor" : "none"} strokeWidth={isFavorite ? 0 : 2} />
    </button>
  );
}

// ─── AlbumArt ───────────────────────────────────────────────────────────────

function AlbumArt({
  cover,
  title,
  className,
  onClick,
}: {
  cover?: string;
  title: string;
  className?: string;
  onClick?: () => void;
}) {
  return (
    <div className={`overflow-hidden flex-shrink-0 ${className ?? ""}`} onClick={onClick}>
      <TidalImage
        src={getTidalImageUrl(cover, 160)}
        alt={title}
        className="w-full h-full object-cover"
      />
    </div>
  );
}

// ─── NarrowTier ─────────────────────────────────────────────────────────────

function NarrowTier({
  track,
  isPlaying,
  sendCommand,
  colors,
}: {
  track: ReturnType<typeof useMiniplayerBridge>["state"]["track"];
  isPlaying: boolean;
  sendCommand: (action: string, value?: number) => void;
  colors: VibrantColors;
}) {
  const title = track ? getTrackDisplayTitle(track) : "";

  return (
    <div className="flex items-center gap-2 w-full h-full px-2">
      <AlbumArt
        cover={track?.album?.cover}
        title={title}
        className="w-12 h-12 rounded-md"
      />
      <div className="flex items-center gap-1 ml-auto flex-shrink-0">
        <PlayButton
          isPlaying={isPlaying}
          onClick={() => sendCommand("toggle-play")}
          size={32}
          colors={colors}
        />
        <button
          onClick={() => sendCommand("play-next")}
          className="w-7 h-7 flex items-center justify-center rounded-full transition-colors duration-150 flex-shrink-0"
          style={{ color: colors.textSecondary }}
        >
          <SkipForward size={15} fill="currentColor" />
        </button>
      </div>
    </div>
  );
}

// ─── CompactTier ────────────────────────────────────────────────────────────

function CompactTier({
  track,
  isPlaying,
  isFavorite,
  playbackSourceLabel,
  sendCommand,
  colors,
}: {
  track: ReturnType<typeof useMiniplayerBridge>["state"]["track"];
  isPlaying: boolean;
  isFavorite: boolean;
  playbackSourceLabel: { type: string; name: string } | null;
  sendCommand: (action: string, value?: number) => void;
  colors: VibrantColors;
}) {
  const title = track ? getTrackDisplayTitle(track) : "";
  const artistName = track?.artist?.name ?? "";

  return (
    <div className="flex items-center gap-2.5 w-full h-full px-2.5">
      <AlbumArt
        cover={track?.album?.cover}
        title={title}
        className="w-14 h-14 rounded-md"
      />
      <div className="flex flex-col justify-center min-w-0 flex-1">
        <span
          className="text-[13px] font-bold truncate leading-tight"
          style={{ color: colors.textPrimary }}
        >
          {title}
        </span>
        <span
          className="text-[11px] truncate mt-0.5"
          style={{ color: colors.textSecondary }}
        >
          {artistName}
        </span>
        {playbackSourceLabel && (
          <span
            className="text-[10px] truncate mt-0.5"
            style={{ color: colors.textMuted }}
          >
            Playing from {playbackSourceLabel.name}
          </span>
        )}
      </div>
      <div className="flex items-center gap-1 flex-shrink-0">
        <FavoriteButton
          isFavorite={isFavorite}
          onClick={() => sendCommand("toggle-favorite")}
          colors={colors}
          size={14}
        />
        <PlayButton
          isPlaying={isPlaying}
          onClick={() => sendCommand("toggle-play")}
          size={32}
          colors={colors}
        />
        <button
          onClick={() => sendCommand("play-next")}
          className="w-7 h-7 flex items-center justify-center rounded-full transition-colors duration-150 flex-shrink-0"
          style={{ color: colors.textSecondary }}
        >
          <SkipForward size={15} fill="currentColor" />
        </button>
      </div>
    </div>
  );
}

// ─── SquareTier ─────────────────────────────────────────────────────────────

function SquareTier({
  track,
  isFavorite,
  playbackSourceLabel,
  sendCommand,
  colors,
}: {
  track: ReturnType<typeof useMiniplayerBridge>["state"]["track"];
  isFavorite: boolean;
  playbackSourceLabel: { type: string; name: string } | null;
  sendCommand: (action: string, value?: number) => void;
  colors: VibrantColors;
}) {
  const title = track ? getTrackDisplayTitle(track) : "";
  const artistName = track?.artist?.name ?? "";

  return (
    <div className="flex flex-col w-full h-full p-3 pt-6">
      <div
        className="w-full aspect-square rounded-lg overflow-hidden cursor-pointer flex-shrink-0"
        onClick={() => sendCommand("show-now-playing")}
      >
        <TidalImage
          src={getTidalImageUrl(track?.album?.cover, 160)}
          alt={title}
          className="w-full h-full object-cover"
        />
      </div>
      <div className="flex flex-col mt-2.5 min-w-0">
        <span
          className="text-[13px] font-bold truncate leading-tight cursor-pointer"
          style={{ color: colors.textPrimary }}
          onClick={() => sendCommand("show-now-playing")}
        >
          {title}
        </span>
        <span
          className="text-[11px] truncate mt-0.5"
          style={{ color: colors.textSecondary }}
        >
          {artistName}
        </span>
        {playbackSourceLabel && (
          <span
            className="text-[10px] truncate mt-0.5"
            style={{ color: colors.textMuted }}
          >
            Playing from {playbackSourceLabel.name}
          </span>
        )}
      </div>
      <div className="flex items-center gap-3 mt-2">
        <FavoriteButton
          isFavorite={isFavorite}
          onClick={() => sendCommand("toggle-favorite")}
          colors={colors}
        />
        <button
          onClick={() => sendCommand("share")}
          className="flex items-center justify-center transition-colors duration-150"
          style={{ color: colors.textSecondary }}
        >
          <Share2 size={15} />
        </button>
      </div>
    </div>
  );
}

// ─── ProgressBar (TallTier) ─────────────────────────────────────────────────

function ProgressBar({
  displayPosition,
  duration,
  sendCommand,
  colors,
}: {
  displayPosition: number;
  duration: number;
  sendCommand: (action: string, value?: number) => void;
  colors: VibrantColors;
}) {
  const barRef = useRef<HTMLDivElement>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [dragProgress, setDragProgress] = useState(0);

  const getProgressFromMouse = useCallback(
    (clientX: number) => {
      const el = barRef.current;
      if (!el || duration <= 0) return 0;
      const rect = el.getBoundingClientRect();
      return Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
    },
    [duration],
  );

  const handleMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      const progress = getProgressFromMouse(e.clientX);
      setDragProgress(progress);
      setIsDragging(true);

      const handleMouseMove = (ev: MouseEvent) => {
        setDragProgress(getProgressFromMouse(ev.clientX));
      };

      const handleMouseUp = (ev: MouseEvent) => {
        const finalProgress = getProgressFromMouse(ev.clientX);
        sendCommand("seek", finalProgress * duration);
        setIsDragging(false);
        window.removeEventListener("mousemove", handleMouseMove);
        window.removeEventListener("mouseup", handleMouseUp);
      };

      window.addEventListener("mousemove", handleMouseMove);
      window.addEventListener("mouseup", handleMouseUp);
    },
    [getProgressFromMouse, duration, sendCommand],
  );

  const progress = isDragging
    ? dragProgress
    : duration > 0
      ? Math.min(1, displayPosition / duration)
      : 0;

  const currentTime = isDragging ? dragProgress * duration : displayPosition;

  const trackColor = colors.isDark ? "rgba(255,255,255,0.2)" : "rgba(0,0,0,0.15)";
  const fillColor = colors.textPrimary;

  return (
    <div className="w-full flex items-center gap-2">
      <span
        className="min-w-[34px] text-right text-[10px] tabular-nums select-none"
        style={{ color: colors.textMuted }}
      >
        {formatTime(currentTime)}
      </span>
      <div
        ref={barRef}
        onMouseDown={handleMouseDown}
        className="flex-1 relative cursor-pointer h-[14px] flex items-center"
      >
        <div className="relative w-full h-[3px] rounded-full" style={{ backgroundColor: trackColor }}>
          <div
            className="absolute left-0 top-0 h-full rounded-full"
            style={{
              width: `${progress * 100}%`,
              backgroundColor: fillColor,
            }}
          />
        </div>
        {isDragging && (
          <div
            className="absolute top-1/2 -translate-y-1/2 w-2.5 h-2.5 rounded-full shadow-md pointer-events-none"
            style={{
              left: `calc(${progress * 100}% - 5px)`,
              backgroundColor: fillColor,
            }}
          />
        )}
      </div>
      <span
        className="min-w-[34px] text-[10px] tabular-nums select-none"
        style={{ color: colors.textMuted }}
      >
        {formatTime(duration)}
      </span>
    </div>
  );
}

// ─── VolumeBar (TallTier) ───────────────────────────────────────────────────

function VolumeBar({
  volume,
  sendVolume,
  colors,
}: {
  volume: number;
  sendVolume: (vol: number) => void;
  colors: VibrantColors;
}) {
  const barRef = useRef<HTMLDivElement>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [dragVolume, setDragVolume] = useState(volume);

  const getVolumeFromMouse = useCallback((clientX: number) => {
    const el = barRef.current;
    if (!el) return 0;
    const rect = el.getBoundingClientRect();
    return Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
  }, []);

  const handleMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      const vol = getVolumeFromMouse(e.clientX);
      setDragVolume(vol);
      setIsDragging(true);
      sendVolume(vol);

      const handleMouseMove = (ev: MouseEvent) => {
        const v = getVolumeFromMouse(ev.clientX);
        setDragVolume(v);
        sendVolume(v);
      };

      const handleMouseUp = () => {
        setIsDragging(false);
        window.removeEventListener("mousemove", handleMouseMove);
        window.removeEventListener("mouseup", handleMouseUp);
      };

      window.addEventListener("mousemove", handleMouseMove);
      window.addEventListener("mouseup", handleMouseUp);
    },
    [getVolumeFromMouse, sendVolume],
  );

  const displayVol = isDragging ? dragVolume : volume;
  const trackColor = colors.isDark ? "rgba(255,255,255,0.2)" : "rgba(0,0,0,0.15)";
  const fillColor = colors.textSecondary;

  return (
    <div className="flex items-center gap-2 w-full">
      <Volume2 size={13} style={{ color: colors.textMuted, flexShrink: 0 }} />
      <div
        ref={barRef}
        onMouseDown={handleMouseDown}
        className="flex-1 relative cursor-pointer h-[14px] flex items-center"
      >
        <div className="relative w-full h-[3px] rounded-full" style={{ backgroundColor: trackColor }}>
          <div
            className="absolute left-0 top-0 h-full rounded-full"
            style={{
              width: `${displayVol * 100}%`,
              backgroundColor: fillColor,
            }}
          />
        </div>
      </div>
    </div>
  );
}

// ─── TallTier ───────────────────────────────────────────────────────────────

function TallTier({
  track,
  isPlaying,
  isFavorite,
  shuffle,
  repeat,
  volume,
  displayPosition,
  duration,
  playbackSourceLabel,
  sendCommand,
  sendVolume,
  colors,
}: {
  track: ReturnType<typeof useMiniplayerBridge>["state"]["track"];
  isPlaying: boolean;
  isFavorite: boolean;
  shuffle: boolean;
  repeat: number;
  volume: number;
  displayPosition: number;
  duration: number;
  playbackSourceLabel: { type: string; name: string } | null;
  sendCommand: (action: string, value?: number) => void;
  sendVolume: (vol: number) => void;
  colors: VibrantColors;
}) {
  const title = track ? getTrackDisplayTitle(track) : "";
  const artistName = track?.artist?.name ?? "";

  const accentColor = colors.isDark ? "#ff6b8a" : "#e5395f";
  const secondaryBtnStyle = { color: colors.textSecondary };

  return (
    <div className="flex flex-col w-full h-full p-3 pt-6">
      {/* Album art */}
      <div
        className="w-full aspect-square rounded-lg overflow-hidden cursor-pointer flex-shrink-0"
        onClick={() => sendCommand("show-now-playing")}
      >
        <TidalImage
          src={getTidalImageUrl(track?.album?.cover, 160)}
          alt={title}
          className="w-full h-full object-cover"
        />
      </div>

      {/* Controls */}
      <div className="flex items-center justify-center gap-3 mt-3">
        {/* Shuffle */}
        <button
          onClick={() => sendCommand("toggle-shuffle")}
          className="w-7 h-7 flex items-center justify-center rounded-full transition-colors duration-200 relative flex-shrink-0"
          style={{ color: shuffle ? accentColor : colors.textSecondary }}
        >
          <Shuffle size={14} strokeWidth={2} />
          {shuffle && (
            <span
              className="absolute -bottom-0.5 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full"
              style={{ backgroundColor: accentColor }}
            />
          )}
        </button>
        {/* Prev */}
        <button
          onClick={() => sendCommand("play-previous")}
          className="w-7 h-7 flex items-center justify-center rounded-full transition-colors duration-150 flex-shrink-0"
          style={secondaryBtnStyle}
        >
          <SkipBack size={16} fill="currentColor" />
        </button>
        {/* Play */}
        <PlayButton
          isPlaying={isPlaying}
          onClick={() => sendCommand("toggle-play")}
          size={38}
          colors={colors}
        />
        {/* Next */}
        <button
          onClick={() => sendCommand("play-next")}
          className="w-7 h-7 flex items-center justify-center rounded-full transition-colors duration-150 flex-shrink-0"
          style={secondaryBtnStyle}
        >
          <SkipForward size={16} fill="currentColor" />
        </button>
        {/* Repeat */}
        <button
          onClick={() => sendCommand("cycle-repeat")}
          className="w-7 h-7 flex items-center justify-center rounded-full transition-colors duration-200 relative flex-shrink-0"
          style={{ color: repeat > 0 ? accentColor : colors.textSecondary }}
        >
          <Repeat size={14} strokeWidth={2} />
          {repeat === 2 && (
            <span
              className="absolute -top-0.5 -right-0.5 text-[7px] font-bold rounded-full w-3 h-3 flex items-center justify-center leading-none"
              style={{ backgroundColor: accentColor, color: colors.bg }}
            >
              1
            </span>
          )}
          {repeat > 0 && (
            <span
              className="absolute -bottom-0.5 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full"
              style={{ backgroundColor: accentColor }}
            />
          )}
        </button>
      </div>

      {/* Progress bar */}
      <div className="mt-2 px-0.5">
        <ProgressBar
          displayPosition={displayPosition}
          duration={duration}
          sendCommand={sendCommand}
          colors={colors}
        />
      </div>

      {/* Volume */}
      <div className="mt-1.5 px-0.5">
        <VolumeBar volume={volume} sendVolume={sendVolume} colors={colors} />
      </div>

      {/* Track info */}
      <div className="flex flex-col mt-2.5 min-w-0">
        <span
          className="text-[13px] font-bold truncate leading-tight cursor-pointer"
          style={{ color: colors.textPrimary }}
          onClick={() => sendCommand("show-now-playing")}
        >
          {title}
        </span>
        <span
          className="text-[11px] truncate mt-0.5"
          style={{ color: colors.textSecondary }}
        >
          {artistName}
        </span>
        {playbackSourceLabel && (
          <span
            className="text-[10px] truncate mt-0.5"
            style={{ color: colors.textMuted }}
          >
            Playing from {playbackSourceLabel.name}
          </span>
        )}
      </div>

      {/* Favorite + Share */}
      <div className="flex items-center gap-3 mt-2">
        <FavoriteButton
          isFavorite={isFavorite}
          onClick={() => sendCommand("toggle-favorite")}
          colors={colors}
        />
        <button
          onClick={() => sendCommand("share")}
          className="flex items-center justify-center transition-colors duration-150"
          style={{ color: colors.textSecondary }}
        >
          <Share2 size={15} />
        </button>
      </div>
    </div>
  );
}

// ─── ErrorOverlay ───────────────────────────────────────────────────────────

function ErrorOverlay({ error }: { error?: string }) {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    if (error) {
      setVisible(true);
      const timer = setTimeout(() => setVisible(false), 4000);
      return () => clearTimeout(timer);
    }
    setVisible(false);
  }, [error]);

  if (!visible || !error) return null;

  return (
    <div className="absolute inset-0 z-30 flex items-center justify-center pointer-events-none">
      <div className="bg-red-900/70 text-white text-[11px] px-3 py-1.5 rounded-md max-w-[90%] truncate">
        {error}
      </div>
    </div>
  );
}

// ─── MiniPlayer ─────────────────────────────────────────────────────────────

export default function MiniPlayer() {
  const containerRef = useRef<HTMLDivElement>(null);
  const tier = useTier(containerRef);
  const { state, displayPosition, isPlaying, sendCommand, sendVolume } = useMiniplayerBridge();
  const colors = useVibrantColors(state.track?.album?.vibrantColor);

  // Crossfade on tier change
  const [opacity, setOpacity] = useState(1);
  const prevTierRef = useRef(tier);

  useEffect(() => {
    if (prevTierRef.current !== tier) {
      prevTierRef.current = tier;
      setOpacity(0);
      const timer = setTimeout(() => setOpacity(1), 30);
      return () => clearTimeout(timer);
    }
  }, [tier]);

  // Keyboard shortcuts
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      switch (e.code) {
        case "Space":
          e.preventDefault();
          sendCommand("toggle-play");
          break;
        case "ArrowRight":
          e.preventDefault();
          sendCommand("play-next");
          break;
        case "ArrowLeft":
          e.preventDefault();
          sendCommand("play-previous");
          break;
        case "Escape":
          getCurrentWindow().close();
          break;
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [sendCommand]);

  const isHorizontal = tier === "narrow" || tier === "compact";

  const renderTier = () => {
    switch (tier) {
      case "narrow":
        return (
          <NarrowTier
            track={state.track}
            isPlaying={isPlaying}
            sendCommand={sendCommand}
            colors={colors}
          />
        );
      case "compact":
        return (
          <CompactTier
            track={state.track}
            isPlaying={isPlaying}
            isFavorite={state.isFavorite}
            playbackSourceLabel={state.playbackSourceLabel}
            sendCommand={sendCommand}
            colors={colors}
          />
        );
      case "square":
        return (
          <SquareTier
            track={state.track}
            isFavorite={state.isFavorite}
            playbackSourceLabel={state.playbackSourceLabel}
            sendCommand={sendCommand}
            colors={colors}
          />
        );
      case "tall":
        return (
          <TallTier
            track={state.track}
            isPlaying={isPlaying}
            isFavorite={state.isFavorite}
            shuffle={state.shuffle}
            repeat={state.repeat}
            volume={state.volume}
            displayPosition={displayPosition}
            duration={state.duration}
            playbackSourceLabel={state.playbackSourceLabel}
            sendCommand={sendCommand}
            sendVolume={sendVolume}
            colors={colors}
          />
        );
    }
  };

  return (
    <div
      ref={containerRef}
      className="group w-full h-full overflow-hidden relative"
      style={{ borderRadius: 12 }}
    >
      {/* Vibrant background */}
      <div
        className="absolute inset-0"
        style={{
          backgroundColor: colors.bg,
          transition: "background-color 500ms ease",
        }}
      />
      {/* Overlay */}
      <div
        className="absolute inset-0"
        style={{ backgroundColor: colors.overlay }}
      />

      {/* Drag handle + close */}
      <DragHandle horizontal={isHorizontal} colors={colors} />
      <CloseButton colors={colors} />

      {/* Error */}
      <ErrorOverlay error={state.error} />

      {/* Content with crossfade */}
      <div
        className="relative z-0 w-full h-full"
        style={{
          opacity,
          transition: "opacity 150ms ease",
        }}
      >
        {renderTier()}
      </div>
    </div>
  );
}
