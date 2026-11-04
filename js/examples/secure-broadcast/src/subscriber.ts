import * as Watch from "@moq/watch";
import { Signal } from "@moq/signals";
import { InMemoryKeyStore, MoqSecureDecrypter } from "moq-secure";

const status = document.querySelector("#status")!;
const subscribeButton =
  document.querySelector<HTMLButtonElement>("#subscribe")!;

let connection: Watch.Net.Connection | undefined;
let player: Watch.Player | undefined;

const canvas = document.createElement("canvas");
canvas.style.display = "block";
canvas.style.width = "100%";
canvas.style.maxWidth = "960px";
canvas.style.background = "black";
document.body.insertBefore(canvas, status);

subscribeButton.addEventListener("click", () => {
  subscribeButton.disabled = true;

  try {
    const url = new URL(
      document.querySelector<HTMLInputElement>("#url")!.value,
    );
    const name = document
      .querySelector<HTMLInputElement>("#name")!
      .value.trim();

    if (!name) throw new Error("Enter a broadcast name.");

    // Must match the publisher's key ID and key bytes.
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
      video: { canvas },
      audio: { muted: new Signal(false) },
      decrypter,
    });

    status.textContent = `Watching ${name}`;
  } catch (error) {
    console.error(error);
    status.textContent =
      `${(error as Error)?.name ?? "Error"}: ${(error as Error)?.message ?? error}`;

    player?.close();
    player = undefined;
    connection?.close();
    connection = undefined;
    subscribeButton.disabled = false;
  }
});
