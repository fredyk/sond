// lib/any/webdav/, WebDAV client and server library (RFC 4918)
//
// WebDAV extends HTTP with methods like PROPFIND, MKCOL, PUT, DELETE, MOVE, COPY, LOCK, UNLOCK.
//
// Files:
//   xml.snd / xml_test.snd , XML builder/parser for PROPFIND multistatus responses
//   client.snd             , WebDAV client (PROPFIND, GET, PUT, MKCOL, DELETE, MOVE, COPY)
//   server.snd             , WebDAV server handler (OPTIONS, PROPFIND, GET, PUT, MKCOL, DELETE)
//   README.md              , this file

// ── Client API (client.snd) ───────────────────────────────────────────────
//
//   dav_connect(host, port) -> fd
//       Connect to <host>:<port>. host is IPv4 dot-string (e.g. "127.0.0.1").
//       Returns fd or -1 on error.
//
//   dav_response_status(resp) -> i64
//       Parse HTTP status code from a raw response. Returns -1 on parse failure.
//
//   dav_is_success(status) -> i64
//       1 if 200 <= status < 300, else 0.
//
//   dav_get_content_length(resp) -> i64
//       Find Content-Length header value in response, or 0.
//
//   dav_response_body(resp, out) -> status
//       Extract body after \r\n\r\n. Writes body to out, returns HTTP status.
//
//   dav_propfind(host, port, path, depth) -> i64
//       Send PROPFIND and return raw response buffer. depth is "0", "1", or "infinity".
//
//   dav_get(host, port, path) -> status
//   dav_put(host, port, path, body) -> status
//   dav_mkcol(host, port, path) -> status
//   dav_delete(host, port, path) -> status
//   dav_move(host, port, src, dst_path) -> status
//   dav_copy(host, port, src, dst_path) -> status
//
// ── Server API (server.snd) ─────────────────────────────────────────────────
//
//   dav_init_file_tree()
//       Initialize the in-memory file tree. Call once at startup.
//
//   dav_add_entry(name, size, is_col)
//       Add an entry (file or collection) to the virtual tree.
//
//   dav_handle(afd, req)
//       Handle one WebDAV request. Writes response to socket fd afd.
//
//   Usage in an accept loop:
//     include "x86_64/linux/http-server.snd"
//     include "any/webdav/server.snd"
//     ...
//       let reqn = read_buf(afd, reqbuf, 8192)
//       if reqn > 0 { dav_handle(afd, bytes_from_raw(reqbuf, reqn)) }
//       http_finish(afd)
//
// ── XML (xml.snd) ────────────────────────────────────────────────────────────
//
// Builders:
//   xml_element(buf, name, value)      , <name>value</name>
//   xml_elem_self(buf, name)            , <name/>
//   xml_propfind_response(buf, ...)     , full <response> block for PROPFIND 207
//   xml_multistatus(buf, entries)        , <multistatus> wrapper
//   xml_build_propfind_body(name)       , PROPFIND request body XML
//
// Parsers:
//   xml_get_tag_content(buf, tag, from) , extract content between <tag>...</tag>
//   xml_has_tag(buf, tag, from)         , 1 if <tag/> self-closing found, else 0
//   xml_extract_hrefs(out, base, buf)  , extract all href values from multistatus
//   xml_count_responses(buf)           , count <response> elements
//   xml_parse_response(xml, pos, out, cf), parse one response block
//