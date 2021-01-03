# Bad Spaceship

[Read about this new game here](https://badspaceship.com).

# Development Updates

- [Meeting Notes](https://gitlab.com/nobo-games/bad-spaceship/-/issues?scope=all&utf8=%E2%9C%93&state=closed&label_name[]=Meeting%20Notes)

# Running

See the instructions for 
https://gitlab.com/nobo-games/templates/web-enabled-game

# Building the Website

The [website](https://badspaceship.com) is automatically built
using [MkDocs](https://www.mkdocs.org/).
It is based on [mkdocs.yml](mkdocs.yml) and the markdown (.md) files in the [docs](docs) subdirectory.

To make changes:

### 1. Setup Python

1. Install Python on your OS.
1. (Recommended): Create a virtual environment.
   From the root of this project, run `python -m venv venv`,
   which will create a virtual environment
   called "venv" in the root of the project.
   The [.gitignore](.gitignore) file is set to ignore this directory,
   as it is specific to you, your machine, and this project.
   Lastly, activate it by running the activation script inside.

### 2. Use MkDocs

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
1. You can commit and push your commits to your branch.
   This won't change the website unless the commits are pushed to the master branch.
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
