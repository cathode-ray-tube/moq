# `@cathode-ray-tube/moq-secure-publish`

A modified fork of [`@moq/publish`](https://www.npmjs.com/package/@moq/publish) that adds encryption and signing through [`@cathode-ray-tube/moq-secure`](https://www.npmjs.com/package/@cathode-ray-tube/moq-secure).

This package captures camera or screen video, encodes it, and publishes it as a named Media over QUIC (MoQ) broadcast. The companion package [`@cathode-ray-tube/moq-secure-watch`](https://www.npmjs.com/package/@cathode-ray-tube/moq-secure-watch) is used to watch encrypted, signed streams.

**Current scope:** publishing video only. Audio publishing is not currently supported.

## Browser usage

The package can be imported directly from a browser using an ES-module CDN such as esm.sh; your application does not need a build step. Pin package versions for reproducible deployments.

```html
<script type="module">
  import * as Publish from "https://esm.sh/@cathode-ray-tube/moq-secure-publish@0.1.0";
  import {
    InMemoryKeyStore,
    MoqSecureEncrypter,
  } from "https://esm.sh/@cathode-ray-tube/moq-secure@0.1.10";

  // Use Publish and the security classes here.
</script>
```

## Example: publish camera or screen video

This page lets the user choose a camera or screen source, previews the captured frames on a canvas, and publishes those same frames.

The relay URL must point to a MoQ relay that accepts publishing connections.

```html
<label>
  Relay URL
  <input id="url" value="https://your-relay.example" />
</label>

<label>
  Broadcast name
  <input id="name" value="my-broadcast" />
</label>

<label>
  Video source
  <select id="source">
    <option value="camera">Camera</option>
    <option value="screen">Screen</option>
  </select>
</label>

<button id="start">Start publishing</button>
<button id="stop" disabled>Stop</button>

<div>
  <canvas id="preview"></canvas>
</div>
<pre id="status"></pre>

<script type="module">
  import * as Publish from "https://esm.sh/@cathode-ray-tube/moq-secure-publish@0.1.0";
  import {
    InMemoryKeyStore,
    MoqSecureEncrypter,
  } from "https://esm.sh/@cathode-ray-tube/moq-secure@0.1.10";

  const status = document.querySelector("#status");
  const startButton = document.querySelector("#start");
  const stopButton = document.querySelector("#stop");
  const previewCanvas = document.querySelector("#preview");

  let connection;
  let origin;
  let broadcast;
  let source;
  let capture;
  let renderer;
  let encoder;

  startButton.addEventListener("click", async () => {
    startButton.disabled = true;

    try {
      const url = new URL(document.querySelector("#url").value);
      const name = document.querySelector("#name").value.trim();
      const sourceKind = document.querySelector("#source").value;

      if (!name) throw new Error("Enter a broadcast name.");

      // Create the source in the click handler so the browser can request
      // camera or screen-capture permission from a user gesture.
      source =
        sourceKind === "screen"
          ? new Publish.Source.Screen({ enabled: true })
          : new Publish.Source.Camera({ enabled: true });

      const video = new Publish.Signals.Computed(
        (effect) => effect.get(source.out.source)?.video,
      );

      const orientation =
        sourceKind === "camera"
          ? source.out.orientation
          : new Publish.Signals.Constant(0);

      capture = new Publish.Video.Capture({
        source: video,
        orientation,
      });

      origin = new Publish.Net.Origin.Producer();

      connection = await Publish.Net.Connection.connect({
        url,
        publish: origin.consume(),
        consume: origin,
      });

      broadcast = new Publish.Broadcast({
        origin,
        enabled: true,
        name: Publish.Net.Path.from(name),
      });

      // TEST KEY ONLY. Replace with a securely generated key and a
      // production-appropriate key store and signing key.
      const keyStore = new InMemoryKeyStore();
      keyStore.setKeyEncoded(
        1,
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
      );

      const encrypter = new MoqSecureEncrypter({
        keyStore,
        signingPrivateKey: new Uint8Array(),
        keyId: 1,
        nSigned: 0,
        padLen: 0,
      });

      encoder = new Publish.Video.Encoder("video/hd", {
        broadcast,
        capture,
        enabled: true,
        encrypter,
      });

      renderer = new Publish.Preview.Renderer({
        canvas: previewCanvas,
        enabled: true,
        mode: "source",
        frames: capture.out.frames,
        display: capture.out.display,
        rotation: capture.out.rotation,
        flip: broadcast.in.flip,
      });

      stopButton.disabled = false;
      status.textContent =
        `Publishing ${name}\nTrack: video/hd\nWaiting for a subscriber…`;
    } catch (error) {
      console.error(error);
      status.textContent =
        `${error?.name ?? "Error"}: ${error?.message ?? error}`;

      renderer?.close();
      encoder?.close();
      capture?.close();
      source?.close();
      connection?.close();

      renderer = undefined;
      encoder = undefined;
      capture = undefined;
      source = undefined;
      connection = undefined;
      broadcast = undefined;
      origin = undefined;

      startButton.disabled = false;
    }
  });

  stopButton.addEventListener("click", () => {
    renderer?.close();
    encoder?.close();
    capture?.close();
    source?.close();
    connection?.close();

    renderer = undefined;
    encoder = undefined;
    capture = undefined;
    source = undefined;
    connection = undefined;
    broadcast = undefined;
    origin = undefined;

    stopButton.disabled = true;
    startButton.disabled = false;
    status.textContent = "Stopped";
  });
</script>
```

## Watching a broadcast

Use [`@cathode-ray-tube/moq-secure-watch`](https://www.npmjs.com/package/@cathode-ray-tube/moq-secure-watch) to watch a stream published by this package. The watch application must be configured with compatible key and signature-verification settings.

## Security notes

- The key in the example is a **test value only**. Do not use it to protect real broadcasts.
- `InMemoryKeyStore` keeps keys in memory; use a production-appropriate key-management approach for real deployments.
- Replace the empty `signingPrivateKey` placeholder with a valid signing key in the format expected by `@cathode-ray-tube/moq-secure`.
- Encryption keys and signature-verification settings must be compatible between publisher and viewer.

## Relationship to `@moq/publish`

This project is a modified fork of `@moq/publish`. It retains the publishing API used by the project and adds encryption and signing support through `@cathode-ray-tube/moq-secure`. It bundles compatible MoQ dependencies required by this fork.

The current package supports publishing **video only**. Audio publishing is not currently supported.
