// Where the rendered file goes. Nothing is sent anywhere; the three options are, in order of
// preference:
//   1. a file the user picks (save picker, Chromium),
//   2. a private file on disk the page owns (Origin Private File System), downloaded when done,
//   3. memory-backed blobs, downloaded when done (last resort; fine for short clips).

import { FileSystemWritableFileStreamTarget, Muxer, StreamTarget } from "mp4-muxer";

type Target = ConstructorParameters<typeof Muxer>[0]["target"];

export interface OutputSink {
  target: Target;
  /** Whether the muxer may write at arbitrary positions. If not, it must produce a fragmented file. */
  seekable: boolean;
  /** Human readable description of where the output goes. */
  description: string;
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
    description: handle.name,
    finish: () => stream.close(),
    abort: () => stream.abort(),
  };
}

function triggerDownload(blob: Blob, fileName: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = fileName;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 120_000);
}

/**
 * Write to a file in the page's private storage area on disk, then hand it to the user as a
 * download. Returns undefined if the browser has no writable private storage.
 */
export async function openPrivateFileSink(fileName: string): Promise<OutputSink | undefined> {
  if (!navigator.storage?.getDirectory) return undefined;
  let handle: FileSystemFileHandle;
  let stream: FileSystemWritableFileStream;
  try {
    const root = await navigator.storage.getDirectory();
    handle = await root.getFileHandle(fileName, { create: true });
    if (typeof handle.createWritable !== "function") return undefined;
    stream = await handle.createWritable();
  } catch {
    return undefined;
  }
  const remove = async () => {
    try {
      const root = await navigator.storage.getDirectory();
      await root.removeEntry(fileName);
    } catch {
      // Already gone or not removable; it only takes space in the page's own storage
    }
  };
  return {
    target: new FileSystemWritableFileStreamTarget(stream),
    seekable: true,
    description: `${fileName} (download when done)`,
    finish: async () => {
      await stream.close();
      triggerDownload(await handle.getFile(), fileName);
      // Give the download time to start reading the file before removing it
      setTimeout(() => void remove(), 10 * 60 * 1000);
    },
    abort: async () => {
      await stream.abort().catch(() => undefined);
      await remove();
    },
  };
}

/** Collect the output in memory-backed blobs and trigger a download at the end. */
export function openMemorySink(fileName: string): OutputSink {
  const parts: Blob[] = [];
  let expected = 0;
  const target = new StreamTarget({
    chunked: true,
    chunkSize: 4 * 1024 * 1024,
    onData: (data, position) => {
      if (position !== expected) {
        throw new Error("Memory output requires sequential writes (use a fragmented file)");
      }
      parts.push(new Blob([data.slice()]));
      expected += data.byteLength;
    },
  });
  return {
    target,
    seekable: false,
    description: `${fileName} (download when done)`,
    finish: async () => {
      triggerDownload(new Blob(parts, { type: "video/mp4" }), fileName);
      parts.length = 0;
    },
    abort: async () => {
      parts.length = 0;
    },
  };
}

/** The best available destination for this browser. Undefined if the user cancelled a picker. */
export async function openBestSink(fileName: string): Promise<OutputSink | undefined> {
  if (canPickSaveLocation()) return openFileSink(fileName);
  return (await openPrivateFileSink(fileName)) ?? openMemorySink(fileName);
}
