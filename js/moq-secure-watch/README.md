# @cathode-ray-tube/moq-secure-watch

A modified fork of [`@moq/watch`](https://www.npmjs.com/package/@moq/watch) for watching MoQ broadcasts with a [`MoQ-Secure`](https://www.npmjs.com/package/@cathode-ray-tube/moq-secure) decrypter.

This package is designed to work with the companion package [`@cathode-ray-tube/moq-secure-publish`](https://www.npmjs.com/package/@cathode-ray-tube/moq-secure-publish). Use the publisher’s example to publish a broadcast, then use this package’s example to watch it. The relay, broadcast name, key ID, key bytes, and signing configuration must match the publisher’s settings.


## Example

Save this as an HTML file and serve it from a local web server. Enter a relay URL and broadcast name, then select **Watch broadcast**.

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>MoQ subscriber</title>
    <style>
      body {
        max-width: 960px;
        margin: 2rem auto;
        padding: 0 1rem;
        font-family: sans-serif;
      }

      label {
        display: block;
        margin: 0.75rem 0;
      }

      input {
        margin-left: 0.5rem;
      }

      canvas {
        display: block;
        width: 100%;
        max-width: 960px;
        aspect-ratio: 16 / 9;
        background: black;
      }

      #status {
        white-space: pre-wrap;
      }
    </style>
  </head>
  <body>
    <h1>MoQ subscriber</h1>

    <label>
      Relay URL
      <input id="url" type="url" placeholder="https://relay.example/path" size="45" />
    </label>

    <label>
      Broadcast name
      <input id="name" value="me.hang" />
    </label>

    <button id="subscribe">Watch broadcast</button>

    <canvas id="video"></canvas>
    <pre id="status">Not watching</pre>

    <script type="module">
      import * as Watch from "https://esm.sh/@cathode-ray-tube/moq-secure-watch";
      import { Signal } from "https://esm.sh/@moq/signals";
      import {
        InMemoryKeyStore,
        MoqSecureDecrypter,
      } from "https://esm.sh/@cathode-ray-tube/moq-secure";

      const status = document.querySelector("#status");
      const subscribeButton = document.querySelector("#subscribe");
      const canvas = document.querySelector("#video");

      let connection;
      let player;

      function closePlayer() {
        player?.close();
        player = undefined;

        connection?.close();
        connection = undefined;
      }

      subscribeButton.addEventListener("click", () => {
        subscribeButton.disabled = true;
        closePlayer();

        try {
          const urlText = document.querySelector("#url").value.trim();
          if (!urlText) {
            throw new Error("Enter a relay URL.");
          }

          const url = new URL(urlText);
          const name = document.querySelector("#name").value.trim();

          if (!name) {
            throw new Error("Enter a broadcast name.");
          }

          // Avoid logging the full URL because its query string might contain credentials.
          console.info("[moq:watch] connecting", {
            url: `${url.origin}${url.pathname}`,
            name,
            at: new Date().toISOString(),
          });

          // This must match the publisher's key ID and key bytes.
          const keyStore = new InMemoryKeyStore();
          keyStore.setKeyEncoded(
            1,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
          );

          // The publisher uses nSigned: 0, so this stream is unsigned.
          const decrypter = new MoqSecureDecrypter({
            keyStore,
            broadcasterPublicKey: new Uint8Array(),
          });

          connection = new Watch.Net.Connection({
            url,
            enabled: true,
          });

          player = new Watch.Player({
            origin: connection.origin,
            probe: connection.probe,
            name: Watch.Net.Path.from(name),
            canvas,
            muted: new Signal(false),
            decrypter,
          });

          status.textContent = `Player created for ${name}; waiting for media.`;
          console.info("[moq:watch] player created", {
            name,
            at: new Date().toISOString(),
          });
        } catch (error) {
          console.error("[moq:watch] setup failed", error);

          status.textContent =
            `${error?.name ?? "Error"}: ${error?.message ?? error}`;

          closePlayer();
          subscribeButton.disabled = false;
        }
      });
    </script>
  </body>
</html>
```

## Configuration notes

- **Relay URL:** Enter the relay URL in the page; no relay URL is preconfigured.
- **Broadcast name:** Use the name of the broadcast you want to watch.
- **Key:** The key ID and key bytes must match the publisher’s configuration.
- **Signing:** This example configures an empty broadcaster public key and assumes the publisher sends unsigned media (`nSigned: 0`). For signed broadcasts, configure the appropriate public key and signing settings.

The key shown in the example is a placeholder. Do not use it for real broadcasts or expose production keys in browser code. Use an appropriate key-distribution and key-management approach for your application.