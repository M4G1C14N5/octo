# Tom's patched Octo: the upstream 2026.11.15 image with /app/octo swapped for this fork's
# build (release tom-2026.11.15.1, see README.md). Built on the server by Coolify from
# https://github.com/M4G1C14N5/octo.git#tom/2026.11.15:deploy/tom
FROM ghcr.io/winters27/octo:2026.11.15
ARG OCTO_TOM_RELEASE=tom-2026.11.15.1
ARG OCTO_TOM_SHA256=__SHA__
ADD --checksum=sha256:${OCTO_TOM_SHA256} \
    https://github.com/M4G1C14N5/octo/releases/download/${OCTO_TOM_RELEASE}/octo-linux-amd64 /app/octo
RUN chmod 0755 /app/octo
LABEL org.opencontainers.image.source="https://github.com/M4G1C14N5/octo" \
      org.opencontainers.image.version="2026.11.15-tom.1"
