import type { CSSProperties } from "react";
import { formatCount, hex } from "../lib/format";
import type { Region } from "../types";

interface SectorStripProps {
  imageSize: number;
  sectorSize: number;
  regions: Region[];
  /** Describes the strip for screen readers. */
  label: string;
  /** Changed bytes per sector, to show a comparison. */
  changed?: number[];
  /** Fraction 0..1 of `range` that has been read, to show a transfer. */
  progress?: number;
  range?: [number, number];
  /** Show which areas the flasher writes. */
  showLegend?: boolean;
  /** Mark areas the flasher never writes. Off while reading, where it makes no difference. */
  showRoles?: boolean;
}

function roleAt(regions: Region[], address: number): Region["role"] {
  return regions.find((r) => address >= r.start && address < r.end)?.role ?? "notWritten";
}

/** Changed bytes are sparse, so scale the fill logarithmically and keep one byte visible. */
function density(changed: number): number {
  if (changed <= 0) return 0;
  return 0.16 + 0.84 * Math.min(1, Math.log10(1 + changed) / 4);
}

export function SectorStrip({
  imageSize,
  sectorSize,
  regions,
  label,
  changed,
  progress,
  range,
  showLegend,
  showRoles = true,
}: SectorStripProps) {
  const cells = Math.ceil(imageSize / sectorSize);
  const reading = progress !== undefined && range !== undefined;
  const reached = reading ? range[0] + progress * (range[1] - range[0]) : 0;

  return (
    <div className="strip-wrap">
      <div className="strip" role="img" aria-label={label} style={{ "--cells": cells } as CSSProperties}>
        {Array.from({ length: cells }, (_, index) => {
          const start = index * sectorSize;
          const end = start + sectorSize;
          const role = showRoles ? roleAt(regions, start) : "written";
          const bytes = changed?.[index] ?? 0;
          const fill = reading ? Math.max(0, Math.min(1, (reached - start) / sectorSize)) : 0;
          const style = { "--fill": fill, "--density": density(bytes) } as CSSProperties;
          const detail = bytes > 0 ? `, ${formatCount(bytes)} bytes changed` : "";
          return (
            <div
              key={index}
              className="cell"
              data-role={role}
              data-ignored={bytes > 0 && role === "notWritten" ? "true" : undefined}
              style={style}
              title={`${hex(start)} to ${hex(end - 1)}${detail}`}
            />
          );
        })}
      </div>
      <div className="ruler" aria-hidden="true">
        {[0, 0.25, 0.5, 0.75, 1].map((f) => (
          <span key={f}>{hex(Math.round(f * imageSize))}</span>
        ))}
      </div>
      {showLegend && (
        <div className="legend">
          <span>
            <i className="swatch" /> Written by the flasher
          </span>
          <span>
            <i className="swatch" data-kind="hatch" /> Never written
          </span>
          {changed && (
            <>
              <span>
                <i className="swatch" data-kind="changed" /> Differs from your backup
              </span>
              <span>
                <i className="swatch" data-kind="ignored" /> Differs, but will be ignored
              </span>
            </>
          )}
        </div>
      )}
    </div>
  );
}
