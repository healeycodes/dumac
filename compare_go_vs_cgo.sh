python3 setup_benchmark.py
go build -o gosyscall previousattamps/gosyscall/main.go
go build -o gocgo previousattamps/fastestgoversion/main.go
hyperfine --warmup 3 --min-runs 3 './gosyscall temp/deep' './gocgo temp/deep'
