module fastzip-cli

go 1.23

require github.com/saracen/fastzip v0.0.0

require (
	github.com/klauspost/compress v1.18.0 // indirect
	github.com/saracen/zipextra v0.0.0-20250129175152-f1aa42d25216 // indirect
	golang.org/x/sync v0.11.0 // indirect
	golang.org/x/sys v0.30.0 // indirect
)

replace github.com/saracen/fastzip => ../fastzip
