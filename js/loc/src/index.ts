/**
 * Low Overhead Container (LOC): encode and decode codec bitstreams framed with
 * per-frame timestamp and timescale metadata for MoQ.
 *
 * @module
 */

import * as Moq from "@moq/net";
import { Time } from "@moq/net";

import type { ContainerFormat } from "./format";
import type { FrameDecrypter } from "../secure/decrypter.js";
import type { FrameEncrypter } from "../secure/encrypter.js";

/** A decoded LOC frame: the codec bitstream plus its timing metadata. */
export interface Frame {
	/** The codec bitstream payload, with the LOC property block stripped. */
	payload: Uint8Array;
	/** Presentation timestamp in microseconds. */
	timestamp: Time.Micro;
	/** True if this frame can be decoded without any preceding frames. */
	keyframe: boolean;
}

const PROP_TIMESCALE = 0x08;
const PROP_TIMESTAMP = 0x10;

// The Timestamp id from draft-ietf-moq-loc-03, accepted on decode only. Draft-03's
// body text and its IANA table disagreed (0x0A vs 0x06); this is the table's value,
// which is what shipped. Draft-04 assigns 0x0A to Secure Objects private properties,
// so it is not accepted here.
const PROP_TIMESTAMP_DRAFT03 = 0x06;

const DEFAULT_TIMESCALE = 1_000_000;

/**
 * Decoder for the Low Overhead Container (LOC) defined in
 * draft-ietf-moq-loc-04.
 *
 * Each MoQ frame is a small property block (timestamp, optional per-frame
 * timescale) followed by the codec bitstream payload. Frames without a 0x08
 * timescale property are interpreted as microseconds.
 */
export class Format implements ContainerFormat {
	/** The kind of content carried by the track. */
	readonly kind: "audio" | "video" | "data";

	#decrypter?: FrameDecrypter;

	/** Configure endpoint metadata for audio or video; opaque data is the default. */
	constructor(kind: "audio" | "video" | "data" = "data") {
		this.kind = kind;
	}

	/**
	 * Configure this format to decrypt complete LOC-container payloads
	 * before decoding them.
	 */
	withDecrypter(decrypter: FrameDecrypter): this {
		this.#decrypter = decrypter;
		return this;
	}

	/** Return the video-frame or audio-source endpoint for an empty codec payload. */
	end(frame: Frame): Time.Micro | undefined {
		return this.kind !== "data" && frame.payload.byteLength === 0
			? frame.timestamp
			: undefined;
	}

	/**
	 * Decode one MoQ frame into its LOC frames.
	 *
	 * Without a decrypter, this remains synchronous. With a decrypter,
	 * the returned promise resolves after decryption and authentication.
	 */
	decode(frame: Uint8Array): Frame[] | Promise<Frame[]> {
		if (!this.#decrypter) {
			return this.#decodePlaintext(frame);
		}

		return this.#decodeEncrypted(frame);
	}

	#decodePlaintext(frame: Uint8Array): Frame[] {
		const [propsLen, afterLen] = Moq.Varint.decode(frame);

		if (afterLen.byteLength < propsLen) {
			throw new Error("loc: properties_length exceeds frame size");
		}

		const props = afterLen.subarray(0, propsLen);
		const payload = afterLen.subarray(propsLen);

		let timestamp: number | undefined;
		let timescale: number | undefined;
		let prevType = 0;
		let first = true;
		let cursor = props;

		while (cursor.byteLength > 0) {
			const [delta, afterDelta] = Moq.Varint.decode(cursor);
			const abs = first ? delta : prevType + delta;

			first = false;
			prevType = abs;
			cursor = afterDelta;

			if (abs % 2 === 0) {
				const [value, afterValue] = Moq.Varint.decode(cursor);
				cursor = afterValue;

				if (
					abs === PROP_TIMESTAMP ||
					abs === PROP_TIMESTAMP_DRAFT03
				) {
					timestamp = value;
				} else if (abs === PROP_TIMESCALE) {
					if (value === 0) {
						throw new Error(
							"loc: timescale property must be non-zero",
						);
					}

					timescale = value;
				}
			} else {
				const [len, afterLenInner] = Moq.Varint.decode(cursor);

				if (afterLenInner.byteLength < len) {
					throw new Error(
						"loc: property length exceeds remaining bytes",
					);
				}

				cursor = afterLenInner.subarray(len);
			}
		}

		if (timestamp === undefined) {
			throw new Error(
				"loc: frame missing required timestamp property",
			);
		}

		const activeTimescale = timescale ?? DEFAULT_TIMESCALE;
		const micros = Math.round(
			(timestamp * DEFAULT_TIMESCALE) / activeTimescale,
		) as Time.Micro;

		return [{
			payload,
			timestamp: micros,
			keyframe: false,
		}];
	}

	async #decodeEncrypted(frame: Uint8Array): Promise<Frame[]> {
		/*
		 * LOC frames do not currently carry a container sequence number.
		 * The secure-frame interface still requires one, so use zero,
		 * matching the legacy-container implementation.
		 */
		const plaintext = await this.#decrypter!.decrypt(0, frame);

		return this.#decodePlaintext(plaintext);
	}
}

/** A payload that can be copied into a buffer without first materializing a Uint8Array. */
export interface Source {
	/** Size in bytes of the payload. */
	byteLength: number;
	/** Copy the payload into the provided buffer. */
	copyTo(buffer: Uint8Array): void;
}

/** Options for a LOC producer. */
export interface ProducerProps {
	// Reserved for future LOC producer options.
}

/**
 * Encoder that packages frames as LOC and writes them to a moq-net track.
 *
 * Each call to {@link encode} produces one MoQ frame containing a
 * property block with the 0x10 timestamp, in microseconds, and the codec
 * bitstream payload.
 */
export class Producer {
	#track: Moq.Track.Producer;
	#group?: Moq.Group.Producer;
	#encrypter?: FrameEncrypter;

	constructor(
		track: Moq.Track.Producer,
		_props: ProducerProps = {},
	) {
		this.#track = track;
	}

	/**
	 * Configure this producer to encrypt complete LOC-container payloads
	 * before writing them to the MoQ track.
	 */
	withEncrypter(encrypter: FrameEncrypter): this {
		this.#encrypter = encrypter;
		return this;
	}

	/**
	 * Encode one frame and write it to the track.
	 *
	 * Without an encrypter, the operation is synchronous. With an encrypter,
	 * the returned promise resolves after encryption and publication.
	 */
	encode(
		data: Uint8Array | Source,
		timestamp: Time.Micro,
		keyframe: boolean,
	): void | Promise<void> {
		if (!this.#encrypter) {
			return this.#encodePlaintext(data, timestamp, keyframe);
		}

		return this.#encodeEncrypted(data, timestamp, keyframe);
	}

	#encodePlaintext(
		data: Uint8Array | Source,
		timestamp: Time.Micro,
		keyframe: boolean,
	): void {
		if (keyframe) {
			this.#group?.close();
			this.#group = this.#track.appendGroup();
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		}

		this.#group.writeFrame({
			payload: this.#encode(data, timestamp),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});
	}

	async #encodeEncrypted(
		data: Uint8Array | Source,
		timestamp: Time.Micro,
		keyframe: boolean,
	): Promise<void> {
		if (keyframe) {
			this.#group?.close();
			this.#group = this.#track.appendGroup();
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		}

		const plaintext = this.#encode(data, timestamp);

		const payload = await this.#encrypter!.encrypt(
			this.#group.sequence,
			plaintext,
		);

		this.#group.writeFrame({
			payload,
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});
	}

	/**
	 * Encode the complete LOC payload before encryption.
	 *
	 * LOC's property block, including the timestamp, is encrypted together
	 * with the codec payload.
	 */
	#encode(
		source: Uint8Array | Source,
		timestamp: Time.Micro,
	): Uint8Array {
		const propTypeBytes = Moq.Varint.encode(PROP_TIMESTAMP);
		const propValueBytes = Moq.Varint.encode(timestamp);
		const propsLen =
			propTypeBytes.byteLength + propValueBytes.byteLength;

		const propsLenBytes = Moq.Varint.encode(propsLen);

		const payloadSize = source.byteLength;
		const total =
			propsLenBytes.byteLength + propsLen + payloadSize;
		const out = new Uint8Array(total);

		let offset = 0;

		out.set(propsLenBytes, offset);
		offset += propsLenBytes.byteLength;

		out.set(propTypeBytes, offset);
		offset += propTypeBytes.byteLength;

		out.set(propValueBytes, offset);
		offset += propValueBytes.byteLength;

		const payloadView = out.subarray(offset);

		if (source instanceof Uint8Array) {
			payloadView.set(source);
		} else {
			source.copyTo(payloadView);
		}

		return out;
	}

	/** Close the current group and the underlying track, optionally with an error. */
	close(err?: Error): void {
		this.#group?.close();
		this.#track.close(err);
	}
}
