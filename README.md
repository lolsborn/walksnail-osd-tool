# Walksnail OSD Tool, web edition

**Hosted at [https://osd.ronoth.com](https://osd.ronoth.com).**

Burn the OSD and SRT telemetry of a Walksnail Avatar HD recording into the video, in your
browser. Pick the `.mp4` and `.osd` from your goggles or VRX (and the `.srt` if you want the
link telemetry), choose a font, check the preview, and render. The result is an H.264 MP4
with the OSD drawn in, the same size and frame rate as the recording.

Everything runs on your own computer. The files you pick are read by the page in your
browser and the rendered video is written back to your disk. Nothing is uploaded: the page's
Content Security Policy forbids it from opening any network connection after it has loaded,
and there is no server side at all.

## Based on the Walksnail OSD Tool

This is a browser port of the [Walksnail OSD Tool](https://github.com/avsaase/walksnail-osd-tool)
by [Alexander van Saase](https://github.com/avsaase), the cross-platform desktop application
for rendering Walksnail OSD and SRT data onto DVR footage. The file parsers, the OSD layout
rules and the SRT line follow the original so that both tools produce the same picture, and
the native tool remains the faster option for very long recordings or batch work. Its source
and build instructions are in this repository too, see [docs/NATIVE_README.md](docs/NATIVE_README.md).

Authors: Alexander van Saase (original project), Steven L Osborn (web edition and native
render pipeline work). Licensed under the GPL-3.0, see [LICENSE.md](LICENSE.md).

## Browser support

- Preview works in any current browser.
- Rendering needs WebCodecs: Chrome, Edge and Brave 94+, Safari 16.4+, Firefox 130+. The
  browser's hardware video codecs are used when it has them.
- Chrome and Edge ask where to save the output. Brave, Safari and Firefox write it to the
  page's private storage and offer it as a download when the render finishes.

## Running it yourself

The web app lives in [`web/`](web/) and is a static site; any web server can host the
build output.

```
cd web
npm install
npm run dev      # development server on http://localhost:5173
npm run build    # production build in web/dist/
npm test
```

Fonts offered in the font dropdown are the PNGs in `web/public/osd-fonts/` listed in
`fonts.json` there. Design notes for the browser version are in
[docs/WEBGPU_BROWSER_SPEC.md](docs/WEBGPU_BROWSER_SPEC.md); performance notes for the native
render pipeline are in [PERFORMANCE_NOTES.md](PERFORMANCE_NOTES.md).
