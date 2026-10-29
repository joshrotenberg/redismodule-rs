# Research environment only; the normal SDK build does not use this image.
FROM rust:1.94.0-bookworm@sha256:365468470075493dc4583f47387001854321c5a8583ea9604b297e67f01c5a4f
RUN apt-get update && apt-get install -y --no-install-recommends python3 procps
# Same source/checksum as .github/servers.json; jemalloc instead of CI's libc.
RUN curl --fail --location --retry 3 -o /tmp/redis.tar.gz https://download.redis.io/releases/redis-8.10.2.tar.gz \
    && printf 'b9ffee226b5eecdba98a679260dad764b2a4ebd90dce4ad5ac9e9f3eef9c02b3  /tmp/redis.tar.gz\n' | sha256sum -c - \
    && mkdir /opt/redis \
    && tar -xzf /tmp/redis.tar.gz --strip-components=1 -C /opt/redis \
    && make -C /opt/redis/src -j2 MALLOC=jemalloc BUILD_TLS=no redis-server redis-cli redis-benchmark \
    && cp /opt/redis/src/redis-server /opt/redis/src/redis-cli /opt/redis/src/redis-benchmark /usr/local/bin/ \
    && redis-server --version
