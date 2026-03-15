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
  VolumeX,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useMiniplayerBridge } from "../hooks/useMiniplayerBridge";
import { getTidalImageUrl, getTrackDisplayTitle } from "../types";
import { formatTime } from "../lib/format";
import TidalImage from "./TidalImage";
import ResizeEdges from "./ResizeEdges";

// ─── Types ──────────────────────────────────────────────────────────────────

type Tier = "narrow" | "compact" | "full";

interface VibrantColors {
  bg: string;
  bgRgba: string;
  overlay: string;
  textPrimary: string;
  textSecondary: string;
  textMuted: string;
  isDark: boolean;
}

// ─── useTier ────────────────────────────────────────────────────────────────

function useTier(ref: RefObject<HTMLDivElement | null>): { tier: Tier; width: number; height: number } {
  const [state, setState] = useState<{ tier: Tier; width: number; height: number }>({
    tier: "full", width: 300, height: 120,
  });

  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const { width, height } = entry.contentRect;

      let tier: Tier;
      if (width < 280) {
        tier = "narrow";
      } else if (height < 180) {
        tier = "compact";
      } else {
        tier = "full";
      }
      setState({ tier, width, height });
    });

    observer.observe(el);
    return () => observer.disconnect();
  }, [ref]);

  return state;
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
      bg: "#0a0a0a",
      bgRgba: "rgba(26,26,26,0.5)",
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
      bg: "#0a0a0a",
      bgRgba: "rgba(26,26,26,0.5)",
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
    bg: "#0a0a0a",
    bgRgba: `rgba(${r},${g},${b},0.5)`,
    overlay: isDark ? "rgba(0,0,0,0.15)" : "rgba(0,0,0,0.05)",
    textPrimary: isDark ? "#ffffff" : "#1a1a1a",
    textSecondary: isDark ? "rgba(255,255,255,0.7)" : "rgba(0,0,0,0.6)",
    textMuted: isDark ? "rgba(255,255,255,0.5)" : "rgba(0,0,0,0.4)",
    isDark,
  };
}

// ─── DragHandle ─────────────────────────────────────────────────────────────

function DragHandle({ tier, colors }: { tier: Tier; colors: VibrantColors }) {
  const dotColor = colors.isDark ? "rgba(255,255,255,0.35)" : "rgba(0,0,0,0.25)";
  const isNarrow = tier === "narrow";

  if (isNarrow) {
    return (
      <div
        data-tauri-drag-region
        className="absolute top-0 left-0 bottom-0 z-10 flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity"
        style={{ width: 20 }}
      >
        <div data-tauri-drag-region className="grid grid-cols-2 grid-rows-3 gap-[2px]">
          {Array.from({ length: 6 }).map((_, i) => (
            <div
              key={i}
              data-tauri-drag-region
              className="w-[3px] h-[3px] rounded-full"
              style={{ backgroundColor: dotColor }}
            />
          ))}
        </div>
      </div>
    );
  }

  return (
    <div
      data-tauri-drag-region
      className="absolute top-0 left-0 right-0 z-10 flex justify-center items-center opacity-0 group-hover:opacity-100 transition-opacity"
      style={{ height: 16 }}
    >
      <div data-tauri-drag-region className="grid grid-cols-3 gap-[2px]">
        {Array.from({ length: 6 }).map((_, i) => (
          <div
            key={i}
            data-tauri-drag-region
            className="w-[3px] h-[3px] rounded-full"
            style={{ backgroundColor: dotColor }}
          />
        ))}
      </div>
    </div>
  );
}

// ─── CloseButton ────────────────────────────────────────────────────────────

function CloseButton({ tier, colors }: { tier: Tier; colors: VibrantColors }) {
  const isNarrow = tier === "narrow";

  return (
    <button
      onClick={() => getCurrentWindow().close()}
      className={`absolute z-20 w-5 h-5 rounded-full flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity ${
        isNarrow ? "top-1 left-0.5" : "top-1 left-1.5"
      }`}
      style={{
        backgroundColor: colors.isDark ? "rgba(255,255,255,0.15)" : "rgba(0,0,0,0.1)",
        color: colors.textPrimary,
      }}
    >
      <X size={11} strokeWidth={2.5} />
    </button>
  );
}

// ─── FavoriteButton ─────────────────────────────────────────────────────────

function FavoriteButton({
  isFavorite,
  onClick,
  colors,
  accentColor,
  size = 16,
}: {
  isFavorite: boolean;
  onClick: () => void;
  colors: VibrantColors;
  accentColor: string;
  size?: number;
}) {
  return (
    <button
      onClick={onClick}
      className="flex items-center justify-center transition-[color,transform] duration-200 active:scale-90 flex-shrink-0"
      style={{
        color: isFavorite ? accentColor : colors.textSecondary,
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
  imageSize,
  onClick,
}: {
  cover?: string;
  title: string;
  className?: string;
  imageSize: number;
  onClick?: () => void;
}) {
  return (
    <div className={`overflow-hidden ${className ?? ""}`} onClick={onClick}>
      <TidalImage
        src={getTidalImageUrl(cover, imageSize)}
        alt={title}
        className="w-full h-full object-cover"
      />
    </div>
  );
}

// ─── ArtOverlayControls ─────────────────────────────────────────────────────

function ArtOverlayControls({
  isPlaying,
  sendCommand,
}: {
  isPlaying: boolean;
  sendCommand: (action: string, value?: number) => void;
}) {
  return (
    <div className="absolute inset-0 flex items-center justify-center gap-6 opacity-0 hover:opacity-100 transition-opacity duration-200 bg-black/40 rounded-lg">
      <button
        onClick={(e) => { e.stopPropagation(); sendCommand("play-previous"); }}
        className="w-9 h-9 flex items-center justify-center text-white/80 hover:text-white transition-colors"
      >
        <SkipBack size={20} fill="currentColor" />
      </button>
      <button
        onClick={(e) => { e.stopPropagation(); sendCommand("toggle-play"); }}
        className="w-12 h-12 rounded-full flex items-center justify-center bg-white/20 hover:bg-white/30 text-white transition-colors backdrop-blur-sm"
      >
        {isPlaying ? <Pause size={24} fill="currentColor" /> : <Play size={24} fill="currentColor" style={{ marginLeft: 2 }} />}
      </button>
      <button
        onClick={(e) => { e.stopPropagation(); sendCommand("play-next"); }}
        className="w-9 h-9 flex items-center justify-center text-white/80 hover:text-white transition-colors"
      >
        <SkipForward size={20} fill="currentColor" />
      </button>
    </div>
  );
}

// ─── VolumePopup ────────────────────────────────────────────────────────────

function VolumePopup({
  volume,
  sendVolume,
  colors,
  onClose,
}: {
  volume: number;
  sendVolume: (vol: number) => void;
  colors: VibrantColors;
  onClose: () => void;
}) {
  const barRef = useRef<HTMLDivElement>(null);

  const getVolumeFromMouse = useCallback((clientY: number) => {
    const el = barRef.current;
    if (!el) return 0;
    const rect = el.getBoundingClientRect();
    return Math.max(0, Math.min(1, 1 - (clientY - rect.top) / rect.height));
  }, []);

  const handleMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const vol = getVolumeFromMouse(e.clientY);
      sendVolume(vol);

      const handleMouseMove = (ev: MouseEvent) => {
        const v = getVolumeFromMouse(ev.clientY);
        sendVolume(v);
      };

      const handleMouseUp = () => {
        window.removeEventListener("mousemove", handleMouseMove);
        window.removeEventListener("mouseup", handleMouseUp);
      };

      window.addEventListener("mousemove", handleMouseMove);
      window.addEventListener("mouseup", handleMouseUp);
    },
    [getVolumeFromMouse, sendVolume],
  );

  const trackColor = colors.isDark ? "rgba(255,255,255,0.2)" : "rgba(0,0,0,0.15)";
  const fillColor = colors.textSecondary;

  return (
    <div
      className="absolute bottom-full left-1/2 -translate-x-1/2 mb-2 rounded-lg p-2 flex flex-col items-center z-30"
      style={{ backgroundColor: colors.isDark ? "rgba(30,30,30,0.95)" : "rgba(240,240,240,0.95)" }}
      onMouseLeave={onClose}
    >
      <div
        ref={barRef}
        onMouseDown={handleMouseDown}
        className="w-[6px] rounded-full relative cursor-pointer"
        style={{ height: 80, backgroundColor: trackColor }}
      >
        <div
          className="absolute bottom-0 left-0 w-full rounded-full"
          style={{
            height: `${volume * 100}%`,
            backgroundColor: fillColor,
          }}
        />
      </div>
    </div>
  );
}

// ─── ProgressBar ────────────────────────────────────────────────────────────

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

// ─── NarrowTier ─────────────────────────────────────────────────────────────

function NarrowTier({
  track,
  isPlaying,
  isFavorite,
  sendCommand,
  colors,
  accentColor,
  containerWidth,
}: {
  track: ReturnType<typeof useMiniplayerBridge>["state"]["track"];
  isPlaying: boolean;
  isFavorite: boolean;
  sendCommand: (action: string, value?: number) => void;
  colors: VibrantColors;
  accentColor: string;
  containerWidth: number;
}) {
  const title = track ? getTrackDisplayTitle(track) : "";
  const artistName = track?.artist?.name ?? "";

  // Progressive controls based on width
  const showNext = containerWidth >= 230;
  const showPrev = containerWidth >= 255;

  return (
    <div className="flex items-center gap-2 w-full h-full px-3 py-2">
      <AlbumArt
        cover={track?.album?.cover}
        title={title}
        className="w-11 h-11 rounded-md flex-shrink-0"
        imageSize={160}
      />
      <div className="flex flex-col justify-center min-w-0 flex-1">
        <span
          className="text-[11px] font-bold truncate leading-tight"
          style={{ color: colors.textPrimary }}
        >
          {title}
        </span>
        <span
          className="text-[10px] truncate mt-0.5"
          style={{ color: colors.textSecondary }}
        >
          {artistName}
        </span>
      </div>
      <div className="flex items-center gap-1 flex-shrink-0">
        <FavoriteButton
          isFavorite={isFavorite}
          onClick={() => sendCommand("toggle-favorite")}
          colors={colors}
          accentColor={accentColor}
          size={14}
        />
        {showPrev && (
          <button
            onClick={() => sendCommand("play-previous")}
            className="w-6 h-6 flex items-center justify-center transition-colors flex-shrink-0"
            style={{ color: colors.textSecondary }}
          >
            <SkipBack size={14} fill="currentColor" />
          </button>
        )}
        <button
          onClick={() => sendCommand("toggle-play")}
          className="w-8 h-8 rounded-full flex items-center justify-center hover:scale-105 active:scale-95 transition-transform flex-shrink-0"
          style={{ backgroundColor: colors.textPrimary, color: colors.bg }}
        >
          {isPlaying ? <Pause size={18} fill="currentColor" /> : <Play size={18} fill="currentColor" style={{ marginLeft: 1 }} />}
        </button>
        {showNext && (
          <button
            onClick={() => sendCommand("play-next")}
            className="w-6 h-6 flex items-center justify-center transition-colors flex-shrink-0"
            style={{ color: colors.textSecondary }}
          >
            <SkipForward size={14} fill="currentColor" />
          </button>
        )}
      </div>
    </div>
  );
}

// ─── CompactTier ────────────────────────────────────────────────────────────

function CompactTier({
  track,
  isPlaying,
  isFavorite,
  shuffle,
  repeat,
  volume,
  playbackSourceLabel,
  sendCommand,
  sendVolume,
  colors,
  accentColor,
  containerHeight,
}: {
  track: ReturnType<typeof useMiniplayerBridge>["state"]["track"];
  isPlaying: boolean;
  isFavorite: boolean;
  shuffle: boolean;
  repeat: number;
  volume: number;
  playbackSourceLabel: { type: string; name: string } | null;
  sendCommand: (action: string, value?: number) => void;
  sendVolume: (vol: number) => void;
  colors: VibrantColors;
  accentColor: string;
  containerHeight: number;
}) {
  const title = track ? getTrackDisplayTitle(track) : "";
  const artistName = track?.artist?.name ?? "";
  const [showVolume, setShowVolume] = useState(false);
  const showPlayingFrom = containerHeight >= 130 && playbackSourceLabel;

  return (
    <div className="flex flex-col w-full h-full px-3 py-2 gap-1.5">
      {/* Row 1: Art + Info + Fav */}
      <div className="flex items-center gap-2.5 min-w-0 flex-1">
        <AlbumArt
          cover={track?.album?.cover}
          title={title}
          className="w-14 h-14 rounded-md flex-shrink-0"
          imageSize={320}
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
          {showPlayingFrom && (
            <span
              className="text-[10px] truncate mt-0.5"
              style={{ color: colors.textMuted }}
            >
              Playing from {playbackSourceLabel.name}
            </span>
          )}
        </div>
        <FavoriteButton
          isFavorite={isFavorite}
          onClick={() => sendCommand("toggle-favorite")}
          colors={colors}
          accentColor={accentColor}
          size={16}
        />
      </div>

      {/* Row 2: Controls */}
      <div className="flex items-center justify-center gap-2 flex-shrink-0">
        <div className="relative flex-shrink-0">
          <button
            onClick={() => setShowVolume((v) => !v)}
            className="w-6 h-6 flex items-center justify-center transition-colors"
            style={{ color: colors.textSecondary }}
          >
            {volume === 0 ? <VolumeX size={14} /> : <Volume2 size={14} />}
          </button>
          {showVolume && (
            <VolumePopup volume={volume} sendVolume={sendVolume} colors={colors} onClose={() => setShowVolume(false)} />
          )}
        </div>
        <button
          onClick={() => sendCommand("toggle-shuffle")}
          className="w-6 h-6 flex items-center justify-center transition-colors flex-shrink-0"
          style={{ color: shuffle ? accentColor : colors.textSecondary }}
        >
          <Shuffle size={14} strokeWidth={2} />
        </button>
        <button
          onClick={() => sendCommand("play-previous")}
          className="w-6 h-6 flex items-center justify-center transition-colors flex-shrink-0"
          style={{ color: colors.textSecondary }}
        >
          <SkipBack size={14} fill="currentColor" />
        </button>
        <button
          onClick={() => sendCommand("toggle-play")}
          className="w-8 h-8 rounded-full flex items-center justify-center hover:scale-105 active:scale-95 transition-transform flex-shrink-0"
          style={{ backgroundColor: colors.textPrimary, color: colors.bg }}
        >
          {isPlaying ? <Pause size={18} fill="currentColor" /> : <Play size={18} fill="currentColor" style={{ marginLeft: 1 }} />}
        </button>
        <button
          onClick={() => sendCommand("play-next")}
          className="w-6 h-6 flex items-center justify-center transition-colors flex-shrink-0"
          style={{ color: colors.textSecondary }}
        >
          <SkipForward size={14} fill="currentColor" />
        </button>
        <button
          onClick={() => sendCommand("cycle-repeat")}
          className="w-6 h-6 flex items-center justify-center transition-colors relative flex-shrink-0"
          style={{ color: repeat > 0 ? accentColor : colors.textSecondary }}
        >
          <Repeat size={14} strokeWidth={2} />
          {repeat === 2 && (
            <span
              className="absolute -top-0.5 -right-0.5 text-[6px] font-bold rounded-full w-2.5 h-2.5 flex items-center justify-center leading-none"
              style={{ backgroundColor: accentColor, color: colors.bg }}
            >
              1
            </span>
          )}
        </button>
        <button
          onClick={() => sendCommand("share")}
          className="w-6 h-6 flex items-center justify-center transition-colors flex-shrink-0"
          style={{ color: colors.textSecondary }}
        >
          <Share2 size={14} />
        </button>
      </div>
    </div>
  );
}

// ─── FullTier ───────────────────────────────────────────────────────────────

function FullTier({
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
  accentColor,
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
  accentColor: string;
}) {
  const title = track ? getTrackDisplayTitle(track) : "";
  const artistName = track?.artist?.name ?? "";
  const [showVolume, setShowVolume] = useState(false);

  const secondaryBtnStyle = { color: colors.textSecondary };

  return (
    <div className="flex flex-col w-full h-full p-3 min-h-0">
      {/* Album art — square, centered */}
      <div className="flex-1 min-h-0 flex items-center justify-center overflow-hidden">
        <div
          className="w-full max-h-full aspect-square rounded-lg overflow-hidden cursor-pointer relative"
          onClick={() => sendCommand("show-now-playing")}
        >
          <TidalImage
            src={getTidalImageUrl(track?.album?.cover, 640)}
            alt={title}
            className="w-full h-full object-cover"
          />
          <ArtOverlayControls isPlaying={isPlaying} sendCommand={sendCommand} />
        </div>
      </div>

      {/* Progress bar */}
      <div className="mt-2 px-0.5 flex-shrink-0">
        <ProgressBar
          displayPosition={displayPosition}
          duration={duration}
          sendCommand={sendCommand}
          colors={colors}
        />
      </div>

      {/* Track info + fav */}
      <div className="flex items-start gap-2 mt-1.5 min-w-0 flex-shrink-0">
        <div className="flex flex-col min-w-0 flex-1">
          <span
            className="text-[15px] font-bold truncate leading-tight cursor-pointer"
            style={{ color: colors.textPrimary }}
            onClick={() => sendCommand("show-now-playing")}
          >
            {title}
          </span>
          <span
            className="text-[12px] truncate mt-0.5"
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
        <FavoriteButton
          isFavorite={isFavorite}
          onClick={() => sendCommand("toggle-favorite")}
          colors={colors}
          accentColor={accentColor}
          size={18}
        />
      </div>

      {/* Controls row — secondary controls only (primary are in art overlay) */}
      <div className="flex items-center justify-center gap-3 mt-1.5 flex-shrink-0">
        <div className="relative flex-shrink-0">
          <button
            onClick={() => setShowVolume((v) => !v)}
            className="w-7 h-7 flex items-center justify-center rounded-full transition-colors duration-200"
            style={secondaryBtnStyle}
          >
            {volume === 0 ? <VolumeX size={16} /> : <Volume2 size={16} />}
          </button>
          {showVolume && (
            <VolumePopup volume={volume} sendVolume={sendVolume} colors={colors} onClose={() => setShowVolume(false)} />
          )}
        </div>
        <button
          onClick={() => sendCommand("toggle-shuffle")}
          className="w-7 h-7 flex items-center justify-center rounded-full transition-colors duration-200 relative flex-shrink-0"
          style={{ color: shuffle ? accentColor : colors.textSecondary }}
        >
          <Shuffle size={16} strokeWidth={2} />
          {shuffle && (
            <span className="absolute -bottom-0.5 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full" style={{ backgroundColor: accentColor }} />
          )}
        </button>
        <button
          onClick={() => sendCommand("cycle-repeat")}
          className="w-7 h-7 flex items-center justify-center rounded-full transition-colors duration-200 relative flex-shrink-0"
          style={{ color: repeat > 0 ? accentColor : colors.textSecondary }}
        >
          <Repeat size={16} strokeWidth={2} />
          {repeat === 2 && (
            <span
              className="absolute -top-0.5 -right-0.5 text-[7px] font-bold rounded-full w-3 h-3 flex items-center justify-center leading-none"
              style={{ backgroundColor: accentColor, color: colors.bg }}
            >
              1
            </span>
          )}
          {repeat > 0 && (
            <span className="absolute -bottom-0.5 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full" style={{ backgroundColor: accentColor }} />
          )}
        </button>
        <button
          onClick={() => sendCommand("share")}
          className="w-7 h-7 flex items-center justify-center rounded-full transition-colors duration-200 flex-shrink-0"
          style={secondaryBtnStyle}
        >
          <Share2 size={16} />
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
  const { tier, width, height } = useTier(containerRef);
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

  const renderTier = () => {
    switch (tier) {
      case "narrow":
        return (
          <NarrowTier
            track={state.track}
            isPlaying={isPlaying}
            isFavorite={state.isFavorite}
            sendCommand={sendCommand}
            colors={colors}
            accentColor={state.accentColor}
            containerWidth={width}
          />
        );
      case "compact":
        return (
          <CompactTier
            track={state.track}
            isPlaying={isPlaying}
            isFavorite={state.isFavorite}
            shuffle={state.shuffle}
            repeat={state.repeat}
            volume={state.volume}
            playbackSourceLabel={state.playbackSourceLabel}
            sendCommand={sendCommand}
            sendVolume={sendVolume}
            colors={colors}
            accentColor={state.accentColor}
            containerHeight={height}
          />
        );
      case "full":
        return (
          <FullTier
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
            accentColor={state.accentColor}
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
      <ResizeEdges />
      {/* Dark base background */}
      <div
        className="absolute inset-0"
        style={{ backgroundColor: colors.bg }}
      />
      {/* Vibrant color at 50% opacity */}
      <div
        className="absolute inset-0"
        style={{
          backgroundColor: colors.bgRgba,
          transition: "background-color 500ms ease",
        }}
      />
      {/* Overlay */}
      <div
        className="absolute inset-0"
        style={{ backgroundColor: colors.overlay }}
      />

      {/* Drag region + close */}
      <DragHandle tier={tier} colors={colors} />
      <CloseButton tier={tier} colors={colors} />

      {/* Error */}
      <ErrorOverlay error={state.error} />

      {/* Content with crossfade + hover padding */}
      <div
        className={`relative z-0 w-full h-full ${
          tier === "narrow"
            ? "pl-0 group-hover:pl-[20px]"
            : "pt-0 group-hover:pt-[16px]"
        }`}
        style={{
          opacity,
          transition: "opacity 150ms ease, padding 200ms ease",
        }}
      >
        {renderTier()}
      </div>
    </div>
  );
}
