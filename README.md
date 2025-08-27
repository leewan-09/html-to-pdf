# HTML to PDF API

Simple API to convert HTML pages to PDF documents.

## Usage

Send a POST request with the URL you want to convert and save the PDF:

```bash
curl --location 'https://pdf.kreating.dev' \
--header 'Content-Type: application/json' \
--data '{
    "name": "event-ticket",
    "url": "https://example.com/tickets/concert-2024"
}' \
--output event-ticket.pdf
```

**Note:** The API returns binary PDF data, so always use `--output` to save it to a file.

## Parameters

- `name`: Name for the generated PDF file
- `url`: URL of the HTML page to convert

## Response

The API returns a PDF file of the specified web page.

## Example URLs

- Event tickets: `https://example.com/tickets/event-123`
- Invoices: `https://example.com/invoice/INV-2024-001`
- Reports: `https://example.com/reports/monthly-summary`
