# Bad Spaceship

[Read about this new game here](https://badspaceship.com).

# Installation

To play Bad Spaceship,
you'll need to clone this repo and choose an implementation below
(the only one so far is Bevy).

## Bevy

[Bevy Engine](https://bevyengine.org/) is a brand-new,
in-development game engine written in the Rust programming language.
Why am I using tools that are so new and unstable?
Because I want to be on the _cutting edge_.

To run this thang:

1. Make sure you have Curl installed.
1. Install [Rust](https://www.rust-lang.org/learn/get-started).
1. If you are on Linux, install the additional Bevy [dependencies](https://github.com/bevyengine/bevy/blob/master/docs/linux_dependencies.md).
1. Clone this repo.
1. (Optional): Enable Fast Compiles for Bevy (you can always skip this and add it later).
   See the instructions per OS [here](https://bevyengine.org/learn/book/getting-started/setup/),
   in the section "Enable Fast Compiles (Optional)".
   As part of this process,
   you'll need to copy [bevy/.cargo/config_fast_builds](bevy/.cargo/config_fast_builds) to bevy/.cargo/config.
   If you're on Mac, see the bottom of that file for more lines to uncomment.
   This increases recompile time by ~40% but removes some debugging features.
1. Navigate in your terminal to the "bevy" subdirectory.
1. Run `cargo run`.

The first time you run this it'll take quite a while to:

1. Download all the dependencies.
1. Compile all the dependencies.
1. Compile the game itself.

But the next time you change something and run `cargo run` again,
it'll finish much faster.

# Building the Website

The [website](https://badspaceship.com) is automatically built
using [MkDocs](https://www.mkdocs.org/).
It is based on [mkdocs.yml](mkdocs.yml) and the markdown (.md) files in the [docs](docs) subdirectory.

To make changes:

1. Install Python on your OS.
1. (Recommended): Create a virtual environment.
   From the root of this project, run `python -m venv venv`,
   which will create a virtual environment
   called "venv" in the root of the project.
   The [.gitignore](.gitignore) file is set to ignore this directory,
   as it is specific to you, your machine, and this project.
   Lastly, activate it by running the activation script inside.
1. Install mkdocs and themes (listed in the [python dependencies file](requirements.txt)):
   `pip install -r requirements.txt`
1. Run `mkdocs serve` to build a temporary version of the site,
   then follow the link printed in the terminal to preview the site in you browser.
1. Switch to a new git branch.
1. Make your changes and save them.
   This site will automatically detect changes to source files
   and reload as long as `mkdocs serve` is running.
1. (Optional): You can also build a more permenant version
   of the site with `mkdocs build`,
   which will put all the generated files a in directory called `site`.
   I'm not sure why you'd want to do that though...
1. When you are ready to push changes to the actual,
   public website,
   commit your changes to your branch,
   switch to the master branch,
   pull any changes to the master branch from the remote repo,
   merge your branch to the master branch,
   and push to the repo.
   Gitlab continuous integration will automatically detect the changes,
   spawn a [Docker](https://hub.docker.com/) container on a Gitlab server,
   install all the prerequisites,
   and rebuild and publish the new version of the site.
   This process is configured in [.gitlab-ci.yml](.gitlab-ci.yml).
