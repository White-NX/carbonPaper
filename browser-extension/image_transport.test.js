import '../browser-extension/image_transport.js';

const transport = globalThis.CarbonPaperImageTransport;

describe('browser extension image transport limits', () => {
  beforeEach(() => {
    globalThis.fetch = async () => ({
      blob: async () => new Blob(['image']),
    });
    globalThis.createImageBitmap = async () => ({
      width: 1,
      height: 1,
      close() {},
    });
  });

  it('measures serialized JSON as UTF-8 bytes', () => {
    expect(transport.utf8JsonBytes({ title: '页面' })).toBe(
      new TextEncoder().encode('{"title":"页面"}').byteLength,
    );
  });

  it('accepts a payload within the Native Messaging budget', async () => {
    const result = await transport.fitPngPayload({
      dataUrl: 'data:image/png;base64,AAAA',
      buildPayload: (imageData) => ({ image_data: imageData }),
    });

    expect(result).not.toBeNull();
    expect(transport.utf8JsonBytes(result.payload)).toBeLessThanOrEqual(
      transport.MAX_REQUEST_BYTES,
    );
  });

  it('scales oversized images to the OCR maximum side before measuring', async () => {
    const canvasSizes = [];
    globalThis.createImageBitmap = async () => ({
      width: 3200,
      height: 1800,
      close() {},
    });
    globalThis.OffscreenCanvas = class {
      constructor(width, height) {
        canvasSizes.push([width, height]);
      }

      getContext() {
        return { drawImage() {} };
      }

      async convertToBlob() {
        return {
          type: 'image/png',
          async arrayBuffer() {
            return new TextEncoder().encode('scaled').buffer;
          },
        };
      }
    };

    const result = await transport.fitPngPayload({
      dataUrl: 'data:image/png;base64,AAAA',
      buildPayload: (imageData) => ({ image_data: imageData }),
    });

    expect(result).not.toBeNull();
    expect(canvasSizes[0]).toEqual([1600, 900]);
  });

  it('rejects a payload that cannot fit even at the smallest scale', async () => {
    const result = await transport.fitPngPayload({
      dataUrl: 'data:image/png;base64,AAAA',
      maxBytes: 1,
      buildPayload: (imageData) => ({ image_data: imageData }),
    });

    expect(result).toBeNull();
  });
});
