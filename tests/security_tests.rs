use html_to_pdf_rust::{PdfRequest, ErrorResponse};
use serde_json;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_validation_allows_http() {
        let json = r#"{"name": "test", "url": "http://example.com"}"#;
        let result: Result<PdfRequest, _> = serde_json::from_str(json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_url_validation_allows_https() {
        let json = r#"{"name": "test", "url": "https://example.com"}"#;
        let result: Result<PdfRequest, _> = serde_json::from_str(json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_url_validation_rejects_file_scheme() {
        let json = r#"{"name": "test", "url": "file:///etc/passwd"}"#;
        let result: Result<PdfRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_url_validation_rejects_javascript_scheme() {
        let json = r#"{"name": "test", "url": "javascript:alert(1)"}"#;
        let result: Result<PdfRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_url_validation_rejects_data_scheme() {
        let json = r#"{"name": "test", "url": "data:text/html,<script>alert(1)</script>"}"#;
        let result: Result<PdfRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_url_validation_rejects_ftp_scheme() {
        let json = r#"{"name": "test", "url": "ftp://example.com/file.txt"}"#;
        let result: Result<PdfRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_url_validation_rejects_invalid_url() {
        let json = r#"{"name": "test", "url": "not-a-url"}"#;
        let result: Result<PdfRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_request_validation_rejects_empty_name() {
        let request = PdfRequest {
            name: "".to_string(),
            url: "https://example.com".to_string(),
        };
        assert!(request.validate().is_err());
    }

    #[test]
    fn test_request_validation_accepts_valid_request() {
        let request = PdfRequest {
            name: "valid-name".to_string(),
            url: "https://example.com".to_string(),
        };
        assert!(request.validate().is_ok());
    }
}

#[cfg(test)]
mod filename_tests {
    use html_to_pdf_rust::sanitize_filename;

    #[test]
    fn test_filename_sanitization_removes_path_traversal() {
        let result = sanitize_filename("../../../etc/passwd");
        assert_eq!(result, ".._.._.._etc_passwd");
    }

    #[test]
    fn test_filename_sanitization_removes_special_chars() {
        let result = sanitize_filename("file<>:\"|?*.pdf");
        assert_eq!(result, "file_______.pdf");
    }

    #[test]
    fn test_filename_sanitization_preserves_safe_chars() {
        let result = sanitize_filename("valid-file_name.123");
        assert_eq!(result, "valid-file_name.123");
    }

    #[test]
    fn test_filename_sanitization_limits_length() {
        let long_name = "a".repeat(100);
        let result = sanitize_filename(&long_name);
        assert_eq!(result.len(), 50);
    }

    #[test]
    fn test_filename_sanitization_handles_empty_input() {
        let result = sanitize_filename("");
        assert_eq!(result, "document");
    }

    #[test]
    fn test_filename_sanitization_handles_only_special_chars() {
        let result = sanitize_filename("!@#$%^&*()");
        assert_eq!(result, "document");
    }

    #[test]
    fn test_filename_sanitization_removes_null_bytes() {
        let result = sanitize_filename("file\0name");
        assert_eq!(result, "file_name");
    }

    #[test]
    fn test_filename_sanitization_removes_control_chars() {
        let result = sanitize_filename("file\x01\x02\x03name");
        assert_eq!(result, "file___name");
    }
}