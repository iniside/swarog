package edge

import "encoding/json"

// Codec encodes and decodes wire values. The default is JSON; msgpack (or any
// other encoding) is a future swap behind this interface — nothing in the
// transport, framing, or RPC dispatch depends on the concrete encoding.
type Codec interface {
	Encode(v any) ([]byte, error)
	Decode(data []byte, v any) error
}

// jsonCodec is the default Codec, backed by encoding/json.
type jsonCodec struct{}

func (jsonCodec) Encode(v any) ([]byte, error) { return json.Marshal(v) }

func (jsonCodec) Decode(data []byte, v any) error { return json.Unmarshal(data, v) }

// defaultCodec is the codec used when a Server/Client is constructed without an
// explicit one.
var defaultCodec Codec = jsonCodec{}
