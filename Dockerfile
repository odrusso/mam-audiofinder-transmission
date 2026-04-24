FROM python:3.12-slim
WORKDIR /app
ARG APP_VERSION=unknown
ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    APP_VERSION=${APP_VERSION}
LABEL org.opencontainers.image.title="MAM Audiobook Finder" \
      org.opencontainers.image.description="Search MyAnonamouse, add audiobooks to Transmission, and import them into Audiobookshelf" \
      org.opencontainers.image.version="${APP_VERSION}"
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates && rm -rf /var/lib/apt/lists/*
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY app/ /app/
EXPOSE 8080
CMD ["uvicorn", "main:app", "--host", "0.0.0.0", "--port", "8080"]
