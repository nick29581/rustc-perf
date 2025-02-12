use benchlib::benchmark::run_benchmark_group;
use tokio::runtime::Builder;
use benchlib::decompress_file;

const TEXT_SHERLOCK: &[u8] = include_bytes!("../../data/sherlock.txt.gz");

async fn total_char_count(reader: String) -> usize{
    let mut total_char = 0;
    
    for line in reader.lines() {
        let mut vec: Vec<&str> = Vec::new();
        total_char += line.chars().count();
        let big :String= line.to_string().repeat(100);
        push_and_pop(&mut vec, &big).await;
    }
    total_char
}


async fn push_and_pop<'a>(vec: &'a mut Vec<&'a str>, line: &'a String){
    vec.push(line.as_str());
    vec.pop().unwrap();
}

async fn async_operation() -> usize {
    let sherlock_text = String::from_utf8(decompress_file(TEXT_SHERLOCK)).expect("Invalid UTF-8");
    let total_char = total_char_count(sherlock_text).await;
    total_char
}

fn main() {
    run_benchmark_group(|group| {
        group.register_benchmark("async-word-count", || {
            // This closure should prepare data that will be needed for the benchmark (if any),
            // and then return a closure that will actually be benchmarked/profiled.
            // Create a Tokio runtime
            let rt = Builder::new_current_thread()
                .enable_all()     
                .build()
                .unwrap(); 
            move || {
                rt.block_on(async_operation())
            }
        });
    });
}


