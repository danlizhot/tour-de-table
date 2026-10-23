fn main() {
    // Icône et métadonnées de l'exécutable Windows.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/tourdetable.ico");
        res.compile().expect("ressources Windows");
    }
}
