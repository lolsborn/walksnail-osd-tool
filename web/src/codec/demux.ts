// MP4 demuxing with mp4box.js, streaming from a File without loading it into memory.
//
// Walksnail DVR files are regular MP4s. The `moov` box (the index) may sit after the `mdat`
// (the media data), so the file is scanned for its top-level boxes first and the index is fed
// to the parser before the media, whatever the order on disk.

import { createFile, DataStream, Endianness, MP4BoxBuffer, type ISOFile, type Sample, type Track } from "mp4box";

export interface VideoTrackInfo {
  id: number;
  /** WebCodecs codec string, e.g. "avc1.640028" or "hvc1.1.6.L153.B0". */
  codec: string;
  width: number;
  height: number;
  timescale: number;
  /** Track duration in seconds. */
  durationSecs: number;
  frameCount: number;
  frameRate: number;
  /** Codec configuration (avcC / hvcC payload) for `VideoDecoder.configure`. */
  description: Uint8Array;
}

export interface AudioTrackInfo {
  id: number;
  codec: string;
  sampleRate: number;
  channelCount: number;
  timescale: number;
  description?: Uint8Array;
}

export interface MediaInfo {
  video: VideoTrackInfo;
  audio?: AudioTrackInfo;
  durationSecs: number;
}

interface BoxLocation {
  type: string;
  start: number;
  size: number;
  /** 16 when the box uses a 64 bit "largesize" header, otherwise 8. */
  headerBytes: number;
}

function mdatHeaderBytes(box: BoxLocation): number {
  return box.headerBytes;
}

const CHUNK_BYTES = 8 * 1024 * 1024;

/** Walk the top-level boxes of an MP4 file by reading only their headers. */
async function scanTopLevelBoxes(file: File): Promise<BoxLocation[]> {
  const boxes: BoxLocation[] = [];
  let offset = 0;
  while (offset + 8 <= file.size) {
    const header = new DataView(await file.slice(offset, offset + 16).arrayBuffer());
    let size = header.getUint32(0);
    const type = String.fromCharCode(header.getUint8(4), header.getUint8(5), header.getUint8(6), header.getUint8(7));
    let headerBytes = 8;
    if (size === 1) {
      // 64 bit "largesize"
      size = Number(header.getBigUint64(8));
      headerBytes = 16;
    } else if (size === 0) {
      size = file.size - offset; // box extends to the end of the file
    }
    if (size < 8) throw new Error(`Corrupt MP4: box ${type} at ${offset} has size ${size}`);
    boxes.push({ type, start: offset, size, headerBytes });
    offset += size;
  }
  return boxes;
}

function codecDescription(isoFile: ISOFile, trackId: number): Uint8Array | undefined {
  const trak = isoFile.getTrackById(trackId);
  // The sample description entries hold the codec configuration box (avcC, hvcC, esds, ...)
  const entries = (trak as unknown as { mdia: { minf: { stbl: { stsd: { entries: unknown[] } } } } }).mdia.minf.stbl.stsd
    .entries;
  for (const entry of entries as Array<Record<string, { write(stream: DataStream): void } | undefined>>) {
    const box = entry.avcC ?? entry.hvcC ?? entry.vpcC ?? entry.av1C;
    if (box) {
      const stream = new DataStream(undefined, 0, Endianness.BIG_ENDIAN);
      box.write(stream);
      // Skip the 8 byte box header; WebCodecs wants the payload only
      return new Uint8Array((stream as unknown as { buffer: ArrayBuffer }).buffer, 8);
    }
  }
  return undefined;
}

function audioDescription(isoFile: ISOFile, trackId: number): Uint8Array | undefined {
  const trak = isoFile.getTrackById(trackId);
  const entries = (trak as unknown as { mdia: { minf: { stbl: { stsd: { entries: unknown[] } } } } }).mdia.minf.stbl.stsd
    .entries;
  for (const entry of entries as Array<{ esds?: { esd?: { descs?: Array<{ descs?: Array<{ data?: Uint8Array }> }> } } }>) {
    // AudioSpecificConfig lives in the DecoderSpecificInfo descriptor inside the esds box
    const specific = entry.esds?.esd?.descs?.[0]?.descs?.[0]?.data;
    if (specific) return specific;
  }
  return undefined;
}

/**
 * A demuxer for one file. `open` parses the index; `samples` then streams the media samples of
 * the video (and audio) track in file order with bounded memory.
 */
export class Demuxer {
  private readonly isoFile: ISOFile;
  private boxes: BoxLocation[] = [];
  private info?: MediaInfo;

  private constructor(private readonly file: File) {
    this.isoFile = createFile();
  }

  static async open(file: File): Promise<Demuxer> {
    const demuxer = new Demuxer(file);
    await demuxer.readIndex();
    return demuxer;
  }

  get media(): MediaInfo {
    if (!this.info) throw new Error("Demuxer not opened");
    return this.info;
  }

  private async appendRange(start: number, end: number): Promise<void> {
    for (let offset = start; offset < end; offset += CHUNK_BYTES) {
      const chunkEnd = Math.min(offset + CHUNK_BYTES, end);
      const buffer = await this.file.slice(offset, chunkEnd).arrayBuffer();
      this.isoFile.appendBuffer(MP4BoxBuffer.fromArrayBuffer(buffer, offset));
    }
  }

  private async readIndex(): Promise<void> {
    this.boxes = await scanTopLevelBoxes(this.file);
    const ready = new Promise<void>((resolve, reject) => {
      this.isoFile.onReady = () => resolve();
      this.isoFile.onError = (module, message) => reject(new Error(`${module}: ${message}`));
    });
    // Feed everything except the media data so the index is parsed first. The parser walks the
    // file in order and needs each mdat's header to know how far to skip, so those bytes go in too.
    for (const box of this.boxes) {
      if (box.type === "mdat") {
        await this.appendRange(box.start, box.start + mdatHeaderBytes(box));
      } else {
        await this.appendRange(box.start, box.start + box.size);
      }
    }
    await ready;

    const movie = this.isoFile.getInfo();
    const videoTrack = movie.videoTracks[0];
    if (!videoTrack) throw new Error("The file has no video track");
    const description = codecDescription(this.isoFile, videoTrack.id);
    if (!description) throw new Error(`Unsupported video codec ${videoTrack.codec}`);
    const video: VideoTrackInfo = {
      id: videoTrack.id,
      codec: videoTrack.codec,
      width: videoTrack.video?.width ?? videoTrack.track_width,
      height: videoTrack.video?.height ?? videoTrack.track_height,
      timescale: videoTrack.timescale,
      durationSecs: videoTrack.duration / videoTrack.timescale,
      frameCount: videoTrack.nb_samples,
      frameRate: videoTrack.nb_samples / (videoTrack.duration / videoTrack.timescale),
      description,
    };
    const audioTrack: Track | undefined = movie.audioTracks[0];
    const audio: AudioTrackInfo | undefined = audioTrack
      ? {
          id: audioTrack.id,
          codec: audioTrack.codec,
          sampleRate: audioTrack.audio?.sample_rate ?? 48000,
          channelCount: audioTrack.audio?.channel_count ?? 2,
          timescale: audioTrack.timescale,
          description: audioDescription(this.isoFile, audioTrack.id),
        }
      : undefined;
    this.info = { video, audio, durationSecs: movie.duration / movie.timescale };
  }

  /**
   * Stream the samples of the given tracks. `onSamples` is awaited for each batch, which is how
   * the caller applies backpressure: the next file chunk is not read until it returns.
   */
  async samples(
    trackIds: number[],
    onSamples: (trackId: number, samples: Sample[]) => Promise<void>,
    shouldStop: () => boolean,
  ): Promise<void> {
    for (const id of trackIds) {
      this.isoFile.setExtractionOptions(id, undefined, { nbSamples: 30 });
    }
    // Serialize delivery: mp4box calls onSamples synchronously from appendBuffer, so the
    // promises are collected and awaited before the next chunk is appended.
    let pending: Promise<void> = Promise.resolve();
    this.isoFile.onSamples = (id, _user, samples) => {
      pending = pending.then(() => onSamples(id, samples)).then(() => {
        const last = samples[samples.length - 1];
        if (last) this.isoFile.releaseUsedSamples(id, last.number);
      });
    };
    this.isoFile.start();
    for (const box of this.boxes) {
      if (box.type !== "mdat") continue;
      const end = box.start + box.size;
      // The header was already fed with the index; buffers must not overlap
      for (let offset = box.start + mdatHeaderBytes(box); offset < end && !shouldStop(); offset += CHUNK_BYTES) {
        const chunkEnd = Math.min(offset + CHUNK_BYTES, end);
        const buffer = await this.file.slice(offset, chunkEnd).arrayBuffer();
        this.isoFile.appendBuffer(MP4BoxBuffer.fromArrayBuffer(buffer, offset), chunkEnd >= this.file.size);
        await pending;
      }
    }
    this.isoFile.flush();
    await pending;
    this.isoFile.stop();
  }
}
