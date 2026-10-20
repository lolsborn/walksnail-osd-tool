# Walksnail OSD Tool for the browser

Renders a Walksnail Avatar DVR with its OSD and SRT telemetry burned in, entirely inside the
browser. Files are read and written locally; the page opens no network connections after
it has loaded (enforced by its Content Security Policy).

Design notes: [`../docs/WEBGPU_BROWSER_SPEC.md`](../docs/WEBGPU_BROWSER_SPEC.md).

## Use

1. Pick the flight files: the `.mp4` and its `.osd`, plus the `.srt` if you want telemetry.
2. Choose an OSD font from the list.
3. Scrub the preview; open Advanced to move the OSD or change the SRT line.
4. Render. Chromium asks where to save; other browsers download the file when it is done.

Output is H.264 at 40 Mbps, the same size and frame rate as the input, with the OSD font
scaled to the video height like the native tool.

## Browser requirements

- Preview: any current browser.
- Render: WebCodecs (Chromium 94+, Safari 16.4+, Firefox 130+). Hardware codecs are used
  when the browser has them.

## Develop

```
npm install
npm run dev        # http://localhost:5173
npm test           # parser tests
npm run build      # type-check + production build in dist/
```

Fonts shipped with the app live in `public/osd-fonts/` and are listed in `fonts.json`
there. Add a Walksnail format font PNG and an entry to offer it in the dropdown.
