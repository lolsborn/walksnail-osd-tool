// Where the rendered file goes. Chromium: straight to a file the user picks. Other browsers:
// collected locally and offered as a download when done. Nothing is sent anywhere.

import { FileSystemWritableFileStreamTarget, Muxer, StreamTarget } from "mp4-muxer";

type Target = ConstructorParameters<typeof Muxer>[0]["target"];

export interface OutputSink {
  target: Target;
  /** Whether the muxer may seek back to place the index at the start. */
  seekable: boolean;
  finish(): Promise<void>;
  abort(): Promise<void>;
}

interface SaveFilePickerWindow {
  showSaveFilePicker?(options: {
    suggestedName?: string;
    types?: Array<{ description: string; accept: Record<string, string[]> }>;
  }): Promise<FileSystemFileHandle>;
}

export function canPickSaveLocation(): boolean {
  return typeof (window as unknown as SaveFilePickerWindow).showSaveFilePicker === "function";
}

/** Ask for a destination file. Returns undefined if the user cancelled. */
export async function openFileSink(suggestedName: string): Promise<OutputSink | undefined> {
  const picker = (window as unknown as SaveFilePickerWindow).showSaveFilePicker;
  if (!picker) return undefined;
  let handle: FileSystemFileHandle;
  try {
    handle = await picker.call(window, {
      suggestedName,
      types: [{ description: "MP4 video", accept: { "video/mp4": [".mp4"] } }],
    });
  } catch (err) {
    if ((err as DOMException).name === "AbortError") return undefined;
    throw err;
  }
  const stream = await handle.createWritable();
  return {
    target: new FileSystemWritableFileStreamTarget(stream),
    seekable: true,
    finish: () => stream.close(),
    abort: () => stream.abort(),
  };
}

/** Collect the output in memory-backed blobs and trigger a download at the end. */
export function openDownloadSink(fileName: string): OutputSink {
  const parts: Blob[] = [];
  let expected = 0;
  const target = new StreamTarget({
    chunked: true,
    chunkSize: 4 * 1024 * 1024,
    onData: (data, position) => {
      if (position !== expected) {
        throw new Error("Download output requires sequential writes");
      }
      parts.push(new Blob([data.slice()]));
      expected += data.byteLength;
    },
  });
  return {
    target,
    seekable: false,
    finish: async () => {
      const blob = new Blob(parts, { type: "video/mp4" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = fileName;
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 60_000);
    },
    abort: async () => {
      parts.length = 0;
    },
  };
}
