from pydantic.json_schema import GenerateJsonSchema as _GenerateJsonSchema


class GenerateJsonSchema(_GenerateJsonSchema):
    """FastAPI's schema generator: `bytes` is a `contentMediaType` string, not `format: binary`."""

    def bytes_schema(self, schema):
        json_schema = {"type": "string", "contentMediaType": "application/octet-stream"}
        bytes_mode = self._config.ser_json_bytes if self.mode == "serialization" else self._config.val_json_bytes
        if bytes_mode == "base64":
            json_schema["contentEncoding"] = "base64"
        self.update_with_validations(json_schema, schema, self.ValidationsMapping.bytes)
        return json_schema
