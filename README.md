# HTML to PDF API

Simple API to convert HTML pages or raw HTML content to PDF documents.

## Endpoints

### 1. URL to PDF (`POST /`)

Convert a web page URL to PDF:

```bash
curl --location 'https://pdf.kreating.dev' \
--header 'Content-Type: application/json' \
--data '{
    "name": "event-ticket",
    "url": "https://example.com/tickets/concert-2024"
}' \
--output event-ticket.pdf
```

### 2. HTML to PDF (`POST /html`)

Convert raw HTML content to PDF:

```bash
curl --location 'https://pdf.kreating.dev/html' \
--header 'Content-Type: application/json' \
--data '{
    "name": "invoice",
    "html": "<!DOCTYPE html><html><head><style>body { font-family: Arial; padding: 40px; }</style></head><body><h1>Invoice #12345</h1><p>Amount: $99.00</p></body></html>"
}' \
--output invoice.pdf
```

### 3. Health Check (`GET /health`)

```bash
curl https://pdf.kreating.dev/health
```

## Parameters

### URL Endpoint (`POST /`)

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | Yes | Name for the generated PDF file (max 256 chars) |
| `url` | string | Yes | URL of the HTML page to convert (http/https only) |
| `options` | object | No | PDF generation options (see below) |

### HTML Endpoint (`POST /html`)

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `name` | string | Yes | Name for the generated PDF file (max 256 chars) |
| `html` | string | Yes | Raw HTML content to convert (max 10 MB) |
| `options` | object | No | PDF generation options (see below) |

### PDF Options

Both endpoints support an optional `options` object:

```json
{
    "options": {
        "format": "A4",
        "printBackground": true,
        "margin": {
            "top": "1cm",
            "right": "1cm",
            "bottom": "1cm",
            "left": "1cm"
        }
    }
}
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `format` | string | `"A4"` | Paper format: `A4`, `Letter`, `Legal`, `A3`, `A5` |
| `printBackground` | boolean | `true` | Include background colors and images |
| `margin` | object | `null` | Page margins (see margin units below) |

### Margin Units

Margins can be specified with different units:
- `"0"` - No margin
- `"1cm"` - Centimeters
- `"10mm"` - Millimeters
- `"0.5in"` - Inches
- `"96px"` - Pixels

## Examples

### URL to PDF with Custom Options

```bash
curl --location 'https://pdf.kreating.dev' \
--header 'Content-Type: application/json' \
--data '{
    "name": "report",
    "url": "https://example.com/reports/monthly",
    "options": {
        "format": "Letter",
        "printBackground": true,
        "margin": {
            "top": "2cm",
            "right": "1.5cm",
            "bottom": "2cm",
            "left": "1.5cm"
        }
    }
}' \
--output report.pdf
```

### HTML to PDF (Full Page, No Margins)

```bash
curl --location 'https://pdf.kreating.dev/html' \
--header 'Content-Type: application/json' \
--data '{
    "name": "certificate",
    "html": "<html><body style=\"margin:0;padding:40px;background:linear-gradient(135deg,#667eea,#764ba2);color:white;height:100vh;display:flex;align-items:center;justify-content:center;\"><h1>Certificate of Achievement</h1></body></html>",
    "options": {
        "format": "A4",
        "printBackground": true,
        "margin": { "top": "0", "right": "0", "bottom": "0", "left": "0" }
    }
}' \
--output certificate.pdf
```

## Response

The API returns binary PDF data. Always use `--output` to save it to a file.

**Success:** Returns PDF file with `Content-Type: application/pdf`

**Error:** Returns JSON with error message:
```json
{
    "error": "Error description"
}
```

## Paper Formats Reference

| Format | Width | Height |
|--------|-------|--------|
| A4 | 8.27 in | 11.69 in |
| Letter | 8.5 in | 11 in |
| Legal | 8.5 in | 14 in |
| A3 | 11.69 in | 16.54 in |
| A5 | 5.83 in | 8.27 in |
