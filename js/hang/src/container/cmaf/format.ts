import type { Time } from "@moq/net";
import type { Format as ContainerFormat } from "../format";
import type { Frame } from "../types";
import type { FrameDecrypter } from "../secure/decrypter.js";
import { decodeDataSegment, type InitSegment } from "./decode";

/** CMAF container format: decodes each MoQ frame as a moof+mdat fragment using the parsed init segment. */
export class Format implements ContainerFormat {
	#init: InitSegment;
	#decrypter?: FrameDecrypter;

	/** Create a format bound to the given parsed init segment (timescale, codec defaults). */
	constructor(init: InitSegment) {
		this.#init = init;
	}

	/**
	 * Configure this format to decrypt CMAF fragments before decoding them.
	 */
	withDecrypter(decrypter: FrameDecrypter): this {
		this.#decrypter = decrypter;
		return this;
	}

	/**
	 * Decode one CMAF fragment into its media frames.
	 *
	 * The plaintext path remains synchronous. If a decrypter is configured,
	 * the result is a promise because frame decryption is asynchronous.
	 */
	decode(frame: Uint8Array): Frame[] | Promise<Frame[]> {
		if (!this.#decrypter) {
			return this.#decodeDataSegment(frame);
		}

		return this.#decodeEncrypted(frame);
	}

	#decodeDataSegment(frame: Uint8Array): Frame[] {
		return decodeDataSegment(frame, this.#init).map((s) => ({
			payload: s.data,
			timestamp: s.timestamp as Time.Micro,
			keyframe: s.keyframe,
			duration: s.duration as Time.Micro,
		}));
	}

	async #decodeEncrypted(frame: Uint8Array): Promise<Frame[]> {
		/*
		 * Use zero for now. If CMAF decryption later requires a sequence
		 * number, this can be changed without altering the decode structure.
		 */
		const plaintext = await this.#decrypter!.decrypt(0, frame);

		return this.#decodeDataSegment(plaintext);
	}
}
