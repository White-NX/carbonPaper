// Helpers for keeping extension screenshot messages below both local IPC
// framing limits. This file is loaded by background.js with importScripts so
// the MV3 service worker can remain a classic (non-module) script.
(function installImageTransport(global) {
  const MAX_NATIVE_MESSAGE_BYTES = 10 * 1024 * 1024;
  const MAX_PIPE_MESSAGE_BYTES = 16 * 1024 * 1024;
  const MAX_REQUEST_BYTES = Math.min(
    MAX_NATIVE_MESSAGE_BYTES,
    MAX_PIPE_MESSAGE_BYTES,
  );
  const MAX_OCR_IMAGE_SIDE = 1600;

  function utf8JsonBytes(value) {
    return new TextEncoder().encode(JSON.stringify(value)).byteLength;
  }

  function splitDataUrl(dataUrl) {
    const separator = dataUrl.indexOf(',');
    if (separator < 0) {
      throw new Error('Invalid image data URL');
    }
    return {
      prefix: dataUrl.slice(0, separator + 1),
      base64: dataUrl.slice(separator + 1),
    };
  }

  async function blobToDataUrl(blob) {
    const buffer = await blob.arrayBuffer();
    let binary = '';
    const bytes = new Uint8Array(buffer);
    const chunkSize = 0x8000;
    for (let offset = 0; offset < bytes.length; offset += chunkSize) {
      binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
    }
    return `data:${blob.type || 'image/png'};base64,${btoa(binary)}`;
  }

  async function resizePngDataUrl(dataUrl, maxSide) {
    const response = await fetch(dataUrl);
    const bitmap = await createImageBitmap(await response.blob());
    try {
      const sourceMaxSide = Math.max(bitmap.width, bitmap.height);
      if (sourceMaxSide <= maxSide) {
        return dataUrl;
      }

      const ratio = maxSide / sourceMaxSide;
      const width = Math.max(1, Math.round(bitmap.width * ratio));
      const height = Math.max(1, Math.round(bitmap.height * ratio));
      const canvas = new OffscreenCanvas(width, height);
      const context = canvas.getContext('2d', { alpha: false });
      if (!context) {
        throw new Error('Cannot create screenshot resize context');
      }
      context.drawImage(bitmap, 0, 0, width, height);
      return await blobToDataUrl(await canvas.convertToBlob({ type: 'image/png' }));
    } finally {
      bitmap.close();
    }
  }

  async function fitPngPayload({
    dataUrl,
    buildPayload,
    maxSide = MAX_OCR_IMAGE_SIDE,
    maxBytes = MAX_REQUEST_BYTES,
  }) {
    let side = Math.max(1, Math.floor(maxSide));
    let candidate = await resizePngDataUrl(dataUrl, side);

    // Re-render from the original data URL on each pass to avoid cumulative
    // resampling blur while progressively reducing the transport size.
    for (let attempt = 0; attempt < 12; attempt += 1) {
      const { base64 } = splitDataUrl(candidate);
      const payload = buildPayload(base64);
      if (utf8JsonBytes(payload) <= maxBytes) {
        return { base64, payload, dataUrl: candidate };
      }

      if (side <= 1) {
        return null;
      }
      side = Math.max(1, Math.floor(side * 0.8));
      candidate = await resizePngDataUrl(dataUrl, side);
    }

    return null;
  }

  global.CarbonPaperImageTransport = Object.freeze({
    MAX_NATIVE_MESSAGE_BYTES,
    MAX_PIPE_MESSAGE_BYTES,
    MAX_REQUEST_BYTES,
    MAX_OCR_IMAGE_SIDE,
    utf8JsonBytes,
    fitPngPayload,
  });
})(globalThis);
