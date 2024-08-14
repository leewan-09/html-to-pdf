FROM --platform=linux/amd64 oven/bun:latest

ENV PUPPETEER_SKIP_CHROMIUM_DOWNLOAD true

RUN apt-get update \
    && apt-get install -y wget gnupg \
    && wget -q -O - https://dl-ssl.google.com/linux/linux_signing_key.pub | apt-key add - \
    && sh -c 'echo "deb [arch=amd64] http://dl.google.com/linux/chrome/deb/ stable main" >> /etc/apt/sources.list.d/google.list' \
    && apt-get update \
    && apt-get install -y google-chrome-stable fonts-ipafont-gothic fonts-wqy-zenhei fonts-thai-tlwg fonts-kacst fonts-freefont-ttf libxss1 \
      --no-install-recommends \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -r app && useradd -rm -g app -G audio,video app

RUN which google-chrome-stable || true

USER app

WORKDIR /home/app

COPY --chown=app:app bun.lockb .
COPY --chown=app:app package.json .

RUN bun install --frozen-lockfile --no-save


COPY --chown=app:app . .

ENV PUPPETEER_SKIP_CHROMIUM_DOWNLOAD=true \
    PUPPETEER_EXECUTABLE_PATH=/usr/bin/google-chrome-stable

CMD ["bun", "./src/index.ts"]