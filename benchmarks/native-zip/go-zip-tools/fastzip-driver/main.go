// Minimal extraction driver for fastzip, which ships no CLI.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"runtime"

	"github.com/saracen/fastzip"
)

func main() {
	threads := flag.Int("concurrency", runtime.GOMAXPROCS(0), "files extracted at once")
	out := flag.String("d", ".", "destination folder")
	flag.Parse()
	if flag.NArg() != 1 {
		fmt.Fprintln(os.Stderr, "usage: fastzip-extract [-concurrency N] [-d DIR] ARCHIVE.zip")
		os.Exit(2)
	}
	if err := os.MkdirAll(*out, 0o777); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	e, err := fastzip.NewExtractor(flag.Arg(0), *out, fastzip.WithExtractorConcurrency(*threads))
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer e.Close()
	if err := e.Extract(context.Background()); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
