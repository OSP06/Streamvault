# ── Stage 1: Build Svelte ─────────────────────────────────────────────────────
FROM node:20-alpine AS builder

WORKDIR /build

# Copy package files and install dependencies & regenerates lock — intentional since lockfile is placeholder
COPY package.json ./
RUN npm install      


# Copy all source and build
COPY . .
RUN npm run build

# ── Stage 2: Serve with nginx ─────────────────────────────────────────────────
FROM nginx:alpine AS runtime

COPY --from=builder /build/build /usr/share/nginx/html
COPY nginx.conf /etc/nginx/conf.d/default.conf

EXPOSE 80
