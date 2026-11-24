# Tom's patched Octo: the upstream 2026.11.15 image with /app/octo swapped for this fork's
# build (release tom-2026.11.15.1, see README.md). Built on the server by Coolify from
# https://github.com/M4G1C14N5/octo.git#tom/2026.11.15:deploy/tom
FROM ghcr.io/winters27/octo:2026.11.15


ADD --checksum=sha256:0dede44284f402c99933ddd6f99b2fa0f10e83a66acb2c065fe98e7d427ede30 \
    https://github.com/M4G1C14N5/octo/releases/download/tom-2026.11.15.1/octo-linux-amd64 /app/octo
RUN chmod 0755 /app/octo
LABEL org.opencontainers.image.source="https://github.com/M4G1C14N5/octo" \
      org.opencontainers.image.version="2026.11.15-tom.1"
