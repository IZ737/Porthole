// A Porthole sidecar backend in Go, standard library only. The handshake
// (see "Auth handshake" in the Porthole README) is the `guard` middleware
// below: copy it into your own server.
//
//	go build -o bin/ .                 # then:
//	porthole run app.toml
//	go run . --standalone              # without Porthole, auth off
package main

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"html/template"
	"log"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/signal"
	"runtime"
	"strings"
	"syscall"
	"time"
)

const cookieName = "app_session"

// guard enforces Porthole's handshake: Host check, ?token= swapped for an
// HttpOnly cookie, 401 for anything without the cookie. An empty token
// (standalone mode) keeps only the Host check.
func guard(port, token string, next http.Handler) http.Handler {
	allowed := map[string]bool{"127.0.0.1:" + port: true, "localhost:" + port: true}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !allowed[strings.ToLower(r.Host)] {
			http.Error(w, "bad Host header", http.StatusForbidden)
			return
		}
		if token == "" {
			next.ServeHTTP(w, r)
			return
		}
		query := r.URL.Query()
		if given, ok := query["token"]; ok && len(given) > 0 && equal(given[0], token) {
			http.SetCookie(w, &http.Cookie{
				Name: cookieName, Value: token, Path: "/",
				HttpOnly: true, SameSite: http.SameSiteStrictMode,
			})
			w.Header().Set("Cache-Control", "no-store")
			http.Redirect(w, r, withoutToken(r.URL), http.StatusFound)
			return
		}
		if c, err := r.Cookie(cookieName); err == nil && equal(c.Value, token) {
			next.ServeHTTP(w, r)
			return
		}
		http.Error(w, "unauthorized", http.StatusUnauthorized)
	})
}

func equal(a, b string) bool {
	return subtle.ConstantTimeCompare([]byte(a), []byte(b)) == 1
}

// withoutToken is the same path with the token parameter removed and any
// other parameters kept as they were.
func withoutToken(u *url.URL) string {
	var kept []string
	for _, part := range strings.Split(u.RawQuery, "&") {
		name, _, _ := strings.Cut(part, "=")
		if decoded, err := url.QueryUnescape(name); err == nil && decoded == "token" {
			continue
		}
		if part != "" {
			kept = append(kept, part)
		}
	}
	path := u.EscapedPath()
	if path == "" || !strings.HasPrefix(path, "/") || strings.HasPrefix(path, "//") {
		path = "/"
	}
	if len(kept) > 0 {
		return path + "?" + strings.Join(kept, "&")
	}
	return path
}

var page = template.Must(template.New("page").Parse(`<!doctype html>
<title>Hello from Go</title>
<style>body { font: 16px system-ui, sans-serif; margin: 3em; }</style>
<h1>Hello from {{.Version}}</h1>
<p>Backend pid {{.Pid}}, listening on 127.0.0.1:{{.Port}}.</p>
<p>Data folder: <code>{{.DataDir}}</code></p>
<p>API call with the session cookie: <b id="api">…</b></p>
<script>
  fetch('/api/ping').then(r => r.json())
    .then(j => { document.getElementById('api').textContent = j.ok ? 'authenticated' : 'failed'; })
    .catch(e => { document.getElementById('api').textContent = 'failed: ' + e; });
</script>
`))

// logged writes "METHOD /path status" to stdout, which Porthole puts in
// sidecar.log. The path only: the first request's query has the token.
type logged struct {
	http.ResponseWriter
	status int
}

func (l *logged) WriteHeader(status int) {
	l.status = status
	l.ResponseWriter.WriteHeader(status)
}

func logRequests(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		lw := &logged{ResponseWriter: w, status: http.StatusOK}
		next.ServeHTTP(lw, r)
		fmt.Printf("%s %s %d\n", r.Method, r.URL.Path, lw.status)
	})
}

func main() {
	standalone := flag.Bool("standalone", false, "run without Porthole (no APP_TOKEN), auth off")
	flag.Parse()

	port := os.Getenv("APP_PORT")
	if port == "" {
		port = "8000"
	}
	token := os.Getenv("APP_TOKEN")
	if token == "" && !*standalone {
		log.Fatal("APP_TOKEN is not set: start this backend with Porthole, or pass --standalone")
	}
	if *standalone {
		log.Print("warning: standalone mode, auth is off")
	}
	dataDir := os.Getenv("APP_DATA_DIR")

	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, r *http.Request) {
		w.Write([]byte("ok"))
	})
	mux.HandleFunc("/api/ping", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]any{"ok": true, "go": runtime.Version(), "pid": os.Getpid()})
	})
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		page.Execute(w, map[string]any{
			"Version": runtime.Version(), "Pid": os.Getpid(), "Port": port, "DataDir": dataDir,
		})
	})

	// Listen on loopback only, never all interfaces.
	listener, err := net.Listen("tcp", "127.0.0.1:"+port)
	if err != nil {
		log.Fatal(err)
	}
	server := &http.Server{Handler: logRequests(guard(port, token, mux))}
	fmt.Printf("listening on 127.0.0.1:%s\n", port)

	// Porthole asks the backend to stop with SIGTERM (Ctrl+Break on Windows).
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt, syscall.SIGTERM)
	go func() {
		<-stop
		fmt.Println("shutting down")
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		server.Shutdown(ctx)
	}()
	if err := server.Serve(listener); err != nil && !errors.Is(err, http.ErrServerClosed) {
		log.Fatal(err)
	}
}
