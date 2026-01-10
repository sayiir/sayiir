pub fn run() {
    println!("Workflow runtime initialized");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_works() {
        run();
    }
}
